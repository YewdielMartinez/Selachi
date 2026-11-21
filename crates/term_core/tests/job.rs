//! Job Object membership of ConPTY shells (`Session::spawn_with_options`), with real `cmd.exe`
//! processes. Every test uses jobs it terminates at the end (or `kill_on_close`), so a failure
//! does not leave `ping.exe` processes behind.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use proc_svc::{
    JobGroup, JobOptions, OwnerMeta, ProcState, Tracker, snapshot, terminate_job,
};
use term_core::{
    JobAttachment, Palette, Session, SessionConfig, SessionEvent, SpawnOptions,
};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

fn config(program: &str, args: &[&str]) -> SessionConfig {
    SessionConfig {
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: None,
        cols: 100,
        rows: 30,
        cell_width: 8,
        cell_height: 16,
        palette: Palette::default(),
        scrollback: 1000,
    }
}

fn job_options(job: &JobGroup) -> SpawnOptions {
    SpawnOptions::default().with_job_handle(job.raw_handle())
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

/// Pid of a `ping.exe` that is a member of `members`.
fn ping_in(members: &[u32]) -> Option<u32> {
    snapshot()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case("ping.exe") && members.contains(&p.pid))
        .map(|p| p.pid)
}

fn alive(pid: u32) -> bool {
    snapshot().iter().any(|p| p.pid == pid)
}

fn kill(pid: u32) {
    // SAFETY: plain Win32 calls on a handle we open and close here.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// Starts `ping` from the shell the way a dev server would be started: through an intermediate
/// `cmd /c start /b`, so its parent exits right away.
fn start_ping(session: &Session) {
    session.write(b"cmd /c start /b ping -n 30 127.0.0.1\r".to_vec());
}

/// Starts a console-detached `cmd /c ping` (own hidden console), the way dev servers and agents
/// launch background work: a child (`cmd`) and a grandchild (`ping`) of a PowerShell shell that
/// do not die together with the shell's pseudoconsole.
fn start_detached(session: &Session) {
    session.write(
        b"Start-Process cmd -WindowStyle Hidden -ArgumentList '/c','ping -n 30 127.0.0.1 > nul'\r"
            .to_vec(),
    );
}

fn powershell() -> SessionConfig {
    config("powershell.exe", &["-NoProfile", "-NoLogo"])
}

/// Terminates the owner's job when dropped (also on a failed assertion).
struct Cleanup(Tracker);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for owner in self.0.owners() {
            if let Some(job) = self.0.job(owner) {
                let _ = job.terminate_raw(1);
            }
        }
    }
}

#[test]
fn shell_and_grandchild_are_born_in_the_job() {
    let job = JobGroup::with_options(
        "born-in-job",
        JobOptions { kill_on_close: true, ..Default::default() },
    )
    .unwrap();
    let (session, _rx) =
        Session::spawn_with_options(config("cmd.exe", &[]), job_options(&job)).expect("spawn");
    let shell = session.pid().expect("shell pid");
    assert_eq!(session.job_attachment(), JobAttachment::AtBirth);
    // Immediately after spawning, no polling needed: the shell was born inside.
    assert!(job.contains(shell), "shell {shell} not in job {:?}", job.members());

    start_ping(&session);
    wait_until("ping in the job", || ping_in(&job.members()).is_some());
    let ping = ping_in(&job.members()).unwrap();
    let members = job.members();
    assert!(members.contains(&shell) && members.contains(&ping), "{members:?}");
}

#[test]
fn grandchild_outlives_the_shell_and_is_reported_orphan() {
    let mut cleanup = Cleanup(Tracker::new());
    let meta = OwnerMeta { agent: None, label: "orphan-test".into(), cwd: None };
    // kill_on_close = false: members survive the job handle, like in the app.
    cleanup.0.create_owner(1, meta, JobOptions::default()).unwrap();
    let handle = cleanup.0.job(1).unwrap().raw_handle();
    let (session, _rx) =
        Session::spawn_with_options(powershell(), SpawnOptions::default().with_job_handle(handle))
            .expect("spawn");
    let shell = session.pid().expect("shell pid");
    cleanup.0.register_root(1, shell).unwrap();

    start_detached(&session);
    wait_until("ping in the job", || ping_in(&cleanup.0.job(1).unwrap().members()).is_some());
    let ping = ping_in(&cleanup.0.job(1).unwrap().members()).unwrap();
    let members = cleanup.0.job(1).unwrap().members();
    assert!(members.len() >= 3, "shell, cmd and ping expected: {members:?}");

    // While the shell lives everything is Active.
    let report = cleanup.0.report().into_iter().find(|r| r.owner == 1).unwrap();
    assert!(report.processes.iter().all(|p| p.state == ProcState::Active), "{report:?}");
    assert!(report.processes.iter().any(|p| p.is_root && p.info.pid == shell));

    // The shell dies (killed, as a crash would).
    kill(shell);
    wait_until("shell exit", || !alive(shell));
    assert!(alive(ping), "ping must survive its shell");
    let report = cleanup.0.report().into_iter().find(|r| r.owner == 1).unwrap();
    let ping_state = report.processes.iter().find(|p| p.info.pid == ping).map(|p| p.state);
    assert_eq!(ping_state, Some(ProcState::Orphan), "{report:?}");
    assert!(report.orphans().any(|p| p.info.pid == ping));

    // Cleaning up the job takes the orphans down.
    let outcome = terminate_job(cleanup.0.job(1).unwrap()).unwrap();
    assert!(outcome.failed.is_empty(), "{outcome:?}");
    wait_until("ping gone", || !alive(ping));
    assert!(cleanup.0.job(1).unwrap().members().is_empty());
    drop(session);
}

#[test]
fn detached_grandchild_survives_dropping_the_session_and_is_reported_orphan() {
    let mut cleanup = Cleanup(Tracker::new());
    let meta = OwnerMeta { agent: None, label: "drop-orphan".into(), cwd: None };
    cleanup.0.create_owner(7, meta, JobOptions::default()).unwrap();
    let handle = cleanup.0.job(7).unwrap().raw_handle();
    let (session, _rx) =
        Session::spawn_with_options(powershell(), SpawnOptions::default().with_job_handle(handle))
            .expect("spawn");
    let shell = session.pid().expect("shell pid");
    cleanup.0.register_root(7, shell).unwrap();
    start_detached(&session);
    wait_until("ping in the job", || ping_in(&cleanup.0.job(7).unwrap().members()).is_some());
    let ping = ping_in(&cleanup.0.job(7).unwrap().members()).unwrap();

    let started = Instant::now();
    drop(session);
    assert!(started.elapsed() < Duration::from_secs(2), "dropping the session blocked");
    wait_until("shell closed with its console", || !alive(shell));
    assert!(alive(ping), "the detached ping must outlive the session");

    let report = cleanup.0.report().into_iter().find(|r| r.owner == 7).unwrap();
    let ping_state = report.processes.iter().find(|p| p.info.pid == ping).map(|p| p.state);
    assert_eq!(ping_state, Some(ProcState::Orphan), "{report:?}");

    terminate_job(cleanup.0.job(7).unwrap()).unwrap();
    wait_until("ping gone", || !alive(ping));
}

#[test]
fn dropping_a_session_with_attached_children_neither_hangs_nor_breaks_the_job() {
    // Dropping the session closes the pseudoconsole: the console clients (shell, and any process
    // still attached to the console) are closed by the console host, but the job itself and the
    // handle stay valid, and the drop neither hangs nor panics.
    let job = JobGroup::with_options(
        "drop-session",
        JobOptions { kill_on_close: true, ..Default::default() },
    )
    .unwrap();
    let (session, _rx) =
        Session::spawn_with_options(config("cmd.exe", &[]), job_options(&job)).expect("spawn");
    let shell = session.pid().unwrap();
    start_ping(&session);
    wait_until("ping in the job", || ping_in(&job.members()).is_some());
    let ping = ping_in(&job.members()).unwrap();
    let started = Instant::now();
    drop(session);
    assert!(started.elapsed() < Duration::from_secs(2), "dropping the session blocked");
    wait_until("shell closed with its console", || !alive(shell));
    // Whether console-attached children survive is up to the console host (observed: they are
    // closed with the pseudoconsole); only report it.
    std::thread::sleep(Duration::from_millis(500));
    println!("attached ping alive after the session was dropped: {}", alive(ping));
    // The job object is still usable.
    assert!(job.try_members().is_ok());
    terminate_job(&job).unwrap();
}

#[test]
fn a_flooding_session_can_be_dropped_without_hanging() {
    // ClosePseudoConsole blocks while output is unread: the drop must not.
    let (session, _rx) = Session::spawn_with_options(
        config("cmd.exe", &["/c", "for /l %i in (1,1,200000) do @echo flood-line-%i"]),
        SpawnOptions::default(),
    )
    .expect("spawn");
    std::thread::sleep(Duration::from_millis(500));
    let shell = session.pid().unwrap();
    let started = Instant::now();
    drop(session);
    assert!(started.elapsed() < Duration::from_secs(2), "drop blocked");
    wait_until("flooding shell gone", || !alive(shell));
}

#[test]
fn an_invalid_job_handle_does_not_fail_the_session() {
    let options = SpawnOptions { job: Some(0x1234), ..Default::default() };
    let (session, _rx) =
        Session::spawn_with_options(config("cmd.exe", &["/c", "echo", "no-job-ok"]), options)
            .expect("the session must start even if the job is unusable");
    assert!(session.pid().is_some());
    assert_eq!(session.job_attachment(), JobAttachment::AssignFailed);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if session.snapshot().text().contains("no-job-ok") {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("shell output missing:\n{}", session.snapshot().text());
}

#[test]
fn env_cwd_and_exit_still_work_with_a_job() {
    let job = JobGroup::with_options(
        "env-cwd",
        JobOptions { kill_on_close: true, ..Default::default() },
    )
    .unwrap();
    let dir = std::env::temp_dir();
    let mut cfg = config("cmd.exe", &["/c", "echo", "%SELACHI_TEST_VAR%-%TERM_PROGRAM%-%CD%"]);
    cfg.cwd = Some(dir.clone());
    let options = SpawnOptions {
        env: vec![("SELACHI_TEST_VAR".to_string(), "from-options".to_string())],
        ..job_options(&job)
    };
    let (session, mut rx) = Session::spawn_with_options(cfg, options).expect("spawn");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline && !exited {
        while let Ok(ev) = rx.try_recv() {
            exited |= matches!(ev, SessionEvent::Exit);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(exited, "no Exit event");
    let text = session.snapshot().text();
    assert!(text.contains("from-options-Selachi-"), "{text}");
    let dir_text = dir.display().to_string();
    assert!(
        text.to_lowercase().contains(&dir_text.trim_end_matches('\\').to_lowercase()),
        "cwd {dir_text} missing in:\n{text}"
    );
}
