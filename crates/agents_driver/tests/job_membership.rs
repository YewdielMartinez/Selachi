//! Job Object membership of spawned agents. No model calls: a harmless stand-in
//! (`cmd /c ping -n 8 127.0.0.1`) goes through the very same code path as the real CLIs
//! (`proc::command_for` + `proc::spawn_in_job` / `AcpDriver::spawn_with`).
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agents_driver::acp::{AcpCommand, AcpDriver};
use agents_driver::{proc, AgentDriver, SpawnOpts, ThreadConfig};
use proc_svc::JobGroup;

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

fn cmd_exe() -> String {
    std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
}

#[test]
fn driver_process_and_descendants_are_job_members() {
    let job = JobGroup::new("agents_driver_test_driver").unwrap();
    let cmd = AcpCommand::new(
        cmd_exe(),
        vec!["/c".into(), "ping -n 8 127.0.0.1 >nul".into()],
    );
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let opts = SpawnOpts {
        job: Some(job.raw_handle() as usize),
    };
    let mut d = AcpDriver::spawn_with(&cmd, &ThreadConfig::new("."), tx, &opts).unwrap();

    let pid = d.pid().expect("driver reports its pid");
    assert!(
        job.contains(pid),
        "agent root {pid} must be in the job, members: {:?}",
        job.members()
    );
    // cmd.exe runs ping.exe as a child: it must be inside the job as well.
    wait_for("a descendant (ping.exe) in the job", || {
        job.members().len() >= 2
    });

    d.shutdown();
}

#[test]
fn without_a_job_nothing_is_assigned_and_pid_is_reported() {
    let job = JobGroup::new("agents_driver_test_nojob").unwrap();
    let cmd = AcpCommand::new(
        cmd_exe(),
        vec!["/c".into(), "ping -n 3 127.0.0.1 >nul".into()],
    );
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let mut d = AcpDriver::spawn(&cmd, &ThreadConfig::new("."), tx).unwrap();
    let pid = d.pid().unwrap();
    assert!(!job.contains(pid));
    d.shutdown();
}

/// npm-style `.cmd` shim that cannot be resolved to a real exe: `cmd.exe` runs the batch file
/// and the real work happens in its children, all of which must land in the job.
#[test]
fn cmd_shim_chain_is_inside_the_job() {
    let dir = std::env::temp_dir().join(format!("agents_driver_job_shim_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shim = dir.join("fakeagent.cmd");
    std::fs::write(&shim, "@echo off\r\nping -n 8 127.0.0.1 >nul\r\n").unwrap();

    let job = JobGroup::new("agents_driver_test_shim").unwrap();
    let mut cmd = proc::command_for(&shim);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = proc::spawn_in_job(&mut cmd, Some(job.raw_handle() as usize)).unwrap();

    assert!(job.contains(child.id()), "shim root must be in the job");
    wait_for("shim descendant (ping.exe) in the job", || {
        job.members().len() >= 2
    });

    proc::kill_tree(child.id());
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The suspended-spawn dance must not break stdio plumbing or exit codes.
#[test]
fn job_spawn_runs_the_process_to_completion() {
    let job = JobGroup::new("agents_driver_test_run").unwrap();
    let mut cmd = Command::new(cmd_exe());
    cmd.args(["/c", "echo hello"]).stdout(Stdio::piped());
    let child = proc::spawn_in_job(&mut cmd, Some(job.raw_handle() as usize)).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello"));
}

/// Live, no model call: the real `claude` (npm shim -> claude.exe) idles on stdin inside the job.
/// Run with `cargo test -p agents_driver --test job_membership -- --ignored`.
#[test]
#[ignore = "needs claude installed; spawns it idle (no model call)"]
fn live_claude_is_inside_the_job() {
    let job = JobGroup::new("agents_driver_test_claude").unwrap();
    let dir = std::env::temp_dir().join("agents_driver_live_claude_job");
    std::fs::create_dir_all(&dir).unwrap();
    let opts = SpawnOpts {
        job: Some(job.raw_handle() as usize),
    };
    let (mut d, _rx) = agents_driver::spawn_with(
        agents_driver::AgentKind::ClaudeCode,
        &ThreadConfig::new(&dir),
        opts,
    )
    .unwrap();
    let pid = d.pid().unwrap();
    assert!(
        job.contains(pid),
        "claude root {pid} not in job: {:?}",
        job.members()
    );
    std::thread::sleep(Duration::from_millis(1500));
    eprintln!("members: {:?}", job.members());
    d.shutdown();
}
