//! Integration tests that spawn real processes. Every job is created with
//! `kill_on_close` and owned by a guard, so children die even if a test fails.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use proc_svc::{
    Attribution, Error, JobEvent, JobGroup, JobOptions, OwnerMeta, ProcState, RefuseReason,
    SafetyPolicy, TerminateOptions, TrackingMode, Tracker, check_safe, listening_ports, snapshot, terminate_job,
    terminate_tree,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn quiet(cmd: &mut Command) -> &mut Command {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
}

fn killing_job(label: &str) -> JobGroup {
    JobGroup::with_options(label, JobOptions { kill_on_close: true, ..Default::default() })
        .expect("job")
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

fn alive(pid: u32) -> bool {
    snapshot().iter().any(|p| p.pid == pid)
}

fn tracker_with_owner(label: &str) -> Tracker {
    let mut t = Tracker::new();
    t.create_owner(
        1,
        OwnerMeta { agent: Some("claude".into()), label: label.into(), cwd: None },
        JobOptions { kill_on_close: true, ..Default::default() },
    )
    .expect("owner");
    t
}

fn ping_cmd() -> Command {
    let mut c = Command::new("cmd");
    c.args(["/c", "ping", "-n", "30", "127.0.0.1"]).creation_flags(CREATE_NO_WINDOW);
    quiet(&mut c);
    c
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for ch in data.chunks(3) {
        let n = (u32::from(ch[0]) << 16)
            | (u32::from(*ch.get(1).unwrap_or(&0)) << 8)
            | u32::from(*ch.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[test]
fn job_members_and_orphan_after_parent_dies() {
    let mut tracker = tracker_with_owner("orphan-test");
    let mut parent = tracker.spawn(1, &mut ping_cmd()).expect("spawn");
    let parent_pid = parent.id();

    // cmd (parent) + ping (child) both end up in the job.
    let job = tracker.job(1).unwrap();
    assert!(wait_until(Duration::from_secs(10), || job.members().len() >= 2), "members: {:?}", job.members());
    assert!(job.contains(parent_pid));

    let report = tracker.report();
    let procs = &report[0].processes;
    assert!(procs.len() >= 2);
    assert!(procs.iter().all(|p| p.state == ProcState::Active), "{procs:?}");
    assert!(procs[0].is_root && procs[0].info.pid == parent_pid);

    // Kill only the parent: the ping child must become an orphan.
    parent.kill().unwrap();
    parent.wait().unwrap();
    let ok = wait_until(Duration::from_secs(10), || {
        tracker.report()[0].processes.iter().any(|p| p.state == ProcState::Orphan)
    });
    let report = tracker.report();
    assert!(ok, "no orphan found: {:?}", report[0].processes);
    let orphan = report[0].processes.iter().find(|p| p.state == ProcState::Orphan).unwrap();
    assert!(orphan.info.name.to_ascii_lowercase().starts_with("ping"));
    assert_ne!(orphan.info.pid, parent_pid);
    assert!(report[0].orphan_bytes() > 0);

    // Cleanup through the safe API.
    let out = terminate_job(tracker.job(1).unwrap()).expect("terminate_job");
    assert!(out.is_clean(), "{out:?}");
    assert!(wait_until(Duration::from_secs(5), || tracker.job(1).unwrap().members().is_empty()));
}

#[test]
fn grandchild_via_start_b_is_in_job_and_orphan() {
    let mut tracker = tracker_with_owner("grandchild-test");
    let mut c = Command::new("cmd");
    c.args(["/c", "start", "/b", "ping", "-n", "30", "127.0.0.1"]).creation_flags(CREATE_NO_WINDOW);
    quiet(&mut c);
    let mut cmd = tracker.spawn(1, &mut c).expect("spawn");
    let _ = cmd.wait(); // cmd exits right after launching ping

    let job = tracker.job(1).unwrap();
    assert!(wait_until(Duration::from_secs(10), || !job.members().is_empty()), "grandchild not in job");
    let ok = wait_until(Duration::from_secs(10), || {
        tracker.report()[0].processes.iter().any(|p| p.state == ProcState::Orphan)
    });
    assert!(ok, "{:?}", tracker.report()[0].processes);

    let out = tracker.terminate_orphans(1, &TerminateOptions::default()).expect("terminate_orphans");
    assert!(out.is_clean(), "{out:?}");
    assert!(wait_until(Duration::from_secs(5), || tracker.job(1).unwrap().members().is_empty()));
}

#[test]
fn listener_becomes_port_zombie_when_parent_exits() {
    let mut tracker = tracker_with_owner("port-test");
    let script = "$l=[Net.Sockets.TcpListener]::new('127.0.0.1',0);$l.Start();Start-Sleep 30";
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut c = Command::new("cmd");
    c.args(["/c", "powershell", "-NoProfile", "-NonInteractive", "-EncodedCommand", &b64(&utf16)])
        .creation_flags(CREATE_NO_WINDOW);
    quiet(&mut c);
    let mut parent = tracker.spawn(1, &mut c).expect("spawn");

    // Wait for the powershell child to listen.
    let listener_pid = {
        let job = tracker.job(1).unwrap();
        let mut found = None;
        let ok = wait_until(Duration::from_secs(30), || {
            let members = job.members();
            found = listening_ports().into_iter().find(|p| members.contains(&p.pid)).map(|p| p.pid);
            found.is_some()
        });
        assert!(ok, "powershell never listened");
        found.unwrap()
    };
    assert_ne!(listener_pid, parent.id());

    // While the root lives the listener is merely Active.
    let report = tracker.report();
    let l = report[0].processes.iter().find(|p| p.info.pid == listener_pid).unwrap();
    assert_eq!(l.state, ProcState::Active);
    assert!(!l.ports.is_empty());

    parent.kill().unwrap();
    parent.wait().unwrap();
    assert!(wait_until(Duration::from_secs(10), || {
        tracker.report()[0]
            .processes
            .iter()
            .any(|p| p.info.pid == listener_pid && p.state == ProcState::PortZombie)
    }), "{:?}", tracker.report()[0].processes);

    let out = terminate_job(tracker.job(1).unwrap()).expect("terminate_job");
    assert!(out.is_clean(), "{out:?}");
    assert!(wait_until(Duration::from_secs(5), || !alive(listener_pid)));
    assert!(!listening_ports().iter().any(|p| p.pid == listener_pid));
}

#[test]
fn terminate_tree_kills_children_first_and_everything() {
    let job = killing_job("tree");
    let mut parent = job.spawn_command(&mut ping_cmd(), 0).expect("spawn");
    let pid = parent.id();
    assert!(wait_until(Duration::from_secs(10), || job.members().len() >= 2));
    let members = job.members();

    let out = terminate_tree(pid, Duration::from_millis(300)).expect("terminate_tree");
    assert!(out.is_clean(), "{out:?}");
    assert_eq!(*out.targeted.last().unwrap(), pid, "parent must come last");
    assert!(out.targeted.len() >= 2);
    let _ = parent.wait();
    assert!(wait_until(Duration::from_secs(5), || members.iter().all(|m| !alive(*m))));
}

#[test]
fn job_notifications_report_new_and_exit() {
    let job = killing_job("notify");
    let mut rx = job.notifications().expect("notifications");
    assert!(matches!(job.notifications(), Err(Error::NotificationsAlreadyEnabled)));

    let mut child = job.spawn_command(&mut ping_cmd(), 0).expect("spawn");
    let pid = child.id();
    let mut events = Vec::new();
    let mut drain = |events: &mut Vec<JobEvent>| {
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
    };
    assert!(wait_until(Duration::from_secs(10), || {
        drain(&mut events);
        events.contains(&JobEvent::NewProcess(pid))
    }), "{events:?}");

    child.kill().unwrap();
    child.wait().unwrap();
    // cmd's ping child also dies? No: only the parent was killed; kill the rest.
    job.terminate_raw(1).unwrap();
    assert!(wait_until(Duration::from_secs(10), || {
        drain(&mut events);
        events.contains(&JobEvent::ExitProcess(pid)) && events.contains(&JobEvent::ActiveProcessZero)
    }), "{events:?}");
}

#[test]
fn safety_refusals() {
    let pol = SafetyPolicy::default();
    for pid in [0u32, 4, std::process::id()] {
        assert!(matches!(check_safe(pid, &pol), Err(Error::Refused { .. })), "pid {pid}");
        assert!(matches!(terminate_tree(pid, Duration::ZERO), Err(Error::Refused { .. })));
    }

    // Critical system processes are refused either by name or because they
    // cannot be opened (access denied => Unverifiable). Both are refusals.
    let procs = snapshot();
    let mut checked = 0;
    for name in ["csrss.exe", "wininit.exe", "services.exe", "lsass.exe", "smss.exe", "winlogon.exe"] {
        for p in procs.iter().filter(|p| p.name.eq_ignore_ascii_case(name)) {
            let r = check_safe(p.pid, &pol);
            assert!(matches!(r, Err(Error::Refused { .. })), "{name} pid {} not refused: {r:?}", p.pid);
            assert!(matches!(terminate_tree(p.pid, Duration::ZERO), Err(Error::Refused { .. })));
            checked += 1;
        }
    }
    assert!(checked > 0, "no critical process found to test");

    // explorer is refused unless explicitly allowed (only check, never kill it).
    if let Some(ex) = procs.iter().find(|p| p.name.eq_ignore_ascii_case("explorer.exe")) {
        match check_safe(ex.pid, &pol) {
            Err(Error::Refused { reason, .. }) => {
                assert!(matches!(reason, RefuseReason::Explorer | RefuseReason::Unverifiable | RefuseReason::OtherSession { .. }))
            }
            other => panic!("explorer not refused: {other:?}"),
        }
        let allowed = SafetyPolicy { allow_explorer: true };
        assert!(!matches!(
            check_safe(ex.pid, &allowed),
            Err(Error::Refused { reason: RefuseReason::Explorer, .. })
        ));
    }
}

#[test]
fn snapshot_has_metrics_for_self() {
    let me = std::process::id();
    let procs = snapshot();
    let p = procs.iter().find(|p| p.pid == me).expect("self in snapshot");
    assert!(p.exe_path.as_deref().is_some_and(|e| e.to_ascii_lowercase().ends_with(".exe")));
    assert!(p.working_set > 0 && p.private_bytes > 0);
    assert!(p.start_time.is_some());
    let cmd = p.cmdline.as_deref().expect("own command line");
    assert!(cmd.to_ascii_lowercase().contains("processes"), "{cmd}");
    assert!(procs.len() > 20);
    assert!(procs.iter().any(|p| p.pid == 0 || p.pid == 4));
}

#[test]
fn heuristic_on_real_system_does_not_panic() {
    let t = Tracker::new();
    let r = t.heuristic_untracked(&["claude", "codex"]);
    assert!(r.iter().all(|u| u.attribution == proc_svc::Attribution::Probable));
}

/// Owns a job-less tracker plus its root; kills everything on drop (even when
/// an assertion fails halfway).
struct TreeGuard {
    tracker: Tracker,
    root: std::process::Child,
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        let opts = TerminateOptions { grace: Duration::ZERO, ..Default::default() };
        let _ = self.tracker.terminate_owner(1, &opts);
        let _ = self.root.kill();
        let _ = self.root.wait();
    }
}

/// A root that detaches a hidden `ping -n 30` (a child that survives the
/// root) and then sleeps. Never put in a job.
fn detaching_root() -> Command {
    let script = "Start-Process -WindowStyle Hidden -FilePath ping -ArgumentList '-n','30','127.0.0.1'; Start-Sleep 30";
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &b64(&utf16)])
        .creation_flags(CREATE_NO_WINDOW);
    quiet(&mut c);
    c
}

#[test]
fn tree_only_owner_tracks_descendants_and_reports_orphan_after_root_dies() {
    let root = detaching_root().spawn().expect("spawn root");
    let root_pid = root.id();
    let mut tracker = Tracker::new();
    tracker.register_root_untracked_job(1, root_pid).expect("register");
    tracker.meta_mut(1).unwrap().label = "tree".into();
    let mut guard = TreeGuard { tracker, root };

    assert_eq!(guard.tracker.attachment_hint(1), TrackingMode::TreeOnly);
    assert_eq!(guard.tracker.attribution(1), Some(Attribution::Tree));
    assert!(guard.tracker.needs_polling());

    // The detached ping shows up as an Active member while the root lives.
    let ping_pid = {
        let mut found = None;
        let ok = wait_until(Duration::from_secs(30), || {
            found = guard.tracker.report()[0]
                .processes
                .iter()
                .find(|p| p.info.name.to_ascii_lowercase().starts_with("ping"))
                .map(|p| p.info.pid);
            found.is_some()
        });
        assert!(ok, "ping never seen: {:?}", guard.tracker.report()[0].processes);
        found.unwrap()
    };
    let report = guard.tracker.report();
    let procs = &report[0].processes;
    assert!(procs[0].is_root && procs[0].info.pid == root_pid, "{procs:?}");
    assert!(procs.iter().all(|p| p.state == ProcState::Active), "{procs:?}");
    // Nothing but descendants of the root (no unrelated processes leaked in).
    assert!(procs.len() <= 4, "{procs:?}");
    assert!(guard.tracker.poll_tree());

    // Kill only the root: the detached ping must become an Orphan.
    guard.root.kill().unwrap();
    guard.root.wait().unwrap();
    let ok = wait_until(Duration::from_secs(10), || {
        guard.tracker.report()[0]
            .processes
            .iter()
            .any(|p| p.info.pid == ping_pid && p.state == ProcState::Orphan)
    });
    assert!(ok, "no orphan: {:?}", guard.tracker.report()[0].processes);
    assert!(guard.tracker.needs_polling());

    // Terminating the orphans (tree-based) cleans up; polling then stops.
    let out = guard.tracker.terminate_orphans(1, &TerminateOptions::default()).expect("terminate");
    assert!(out.is_clean(), "{out:?}");
    assert!(wait_until(Duration::from_secs(5), || !alive(ping_pid)));
    assert!(wait_until(Duration::from_secs(5), || !guard.tracker.poll_tree()));
}

#[test]
fn tree_only_late_report_still_finds_orphan() {
    // No report/poll between spawn and root death: the orphan is still found
    // through the dead root's remembered identity.
    let root = detaching_root().spawn().expect("spawn root");
    let mut tracker = Tracker::new();
    tracker.register_root_untracked_job(1, root.id()).expect("register");
    let mut guard = TreeGuard { tracker, root };
    // Let ping start, then take the root down without reporting.
    assert!(wait_until(Duration::from_secs(30), || {
        snapshot().iter().any(|p| {
            p.ppid == guard.root.id() && p.name.to_ascii_lowercase().starts_with("ping")
        })
    }));
    guard.root.kill().unwrap();
    guard.root.wait().unwrap();
    let ok = wait_until(Duration::from_secs(10), || {
        guard.tracker.report()[0].processes.iter().any(|p| {
            p.state == ProcState::Orphan && p.info.name.to_ascii_lowercase().starts_with("ping")
        })
    });
    assert!(ok, "{:?}", guard.tracker.report()[0].processes);
}
