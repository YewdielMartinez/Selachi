//! Running the real `git` CLI: no console window, no optional locks, quotepath off, with a timeout.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Default timeout for local (read) operations.
pub(crate) const LOCAL_TIMEOUT: Duration = Duration::from_secs(20);
/// Timeout for operations that may touch the network.
pub(crate) const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) struct Raw {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Raw {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }
}

fn display_args<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extra knobs for [`run_git_with`].
#[derive(Default)]
pub(crate) struct Opts<'a> {
    /// Extra environment variables for the child.
    pub envs: Vec<(&'a str, std::ffi::OsString)>,
    /// Bytes piped to the child's stdin (`None` = closed stdin).
    pub stdin: Option<&'a [u8]>,
}

/// Run `git <args>` in `cwd` and capture its output. A non-zero exit is NOT an error here.
pub(crate) fn run_git<S: AsRef<OsStr>>(cwd: &Path, args: &[S], timeout: Duration) -> Result<Raw> {
    run_git_with(cwd, args, &Opts::default(), timeout)
}

/// [`run_git`] with environment overrides and optional stdin.
pub(crate) fn run_git_with<S: AsRef<OsStr>>(
    cwd: &Path,
    args: &[S],
    opts: &Opts<'_>,
    timeout: Duration,
) -> Result<Raw> {
    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.quotepath=false")
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if opts.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &opts.envs {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::GitNotFound(e)
        } else {
            Error::Io(e)
        }
    })?;

    let in_t = opts.stdin.and_then(|data| {
        let data = data.to_vec();
        child.stdin.take().map(|mut si| {
            thread::spawn(move || {
                use std::io::Write;
                let _ = si.write_all(&data);
            })
        })
    });
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_t = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = out.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let err_t = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = err.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut sleep = Duration::from_millis(1);
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(sleep);
                sleep = (sleep * 2).min(Duration::from_millis(20));
            }
        }
    };

    if let Some(t) = in_t {
        let _ = t.join();
    }
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    match status {
        Some(status) => Ok(Raw {
            code: status.code(),
            stdout,
            stderr,
        }),
        None => Err(Error::Timeout {
            args: display_args(args),
            timeout,
        }),
    }
}

/// Like [`run_git`] but a non-zero exit becomes [`Error::Git`].
pub(crate) fn run_git_ok<S: AsRef<OsStr>>(cwd: &Path, args: &[S]) -> Result<Raw> {
    let raw = run_git(cwd, args, LOCAL_TIMEOUT)?;
    if raw.success() {
        Ok(raw)
    } else {
        Err(Error::Git {
            args: display_args(args),
            code: raw.code,
            stderr: raw.stderr_string(),
        })
    }
}

/// Like [`run_git_with`] but a non-zero exit becomes [`Error::Git`].
pub(crate) fn run_git_ok_with<S: AsRef<OsStr>>(
    cwd: &Path,
    args: &[S],
    opts: &Opts<'_>,
    timeout: Duration,
) -> Result<Raw> {
    let raw = run_git_with(cwd, args, opts, timeout)?;
    if raw.success() {
        Ok(raw)
    } else {
        Err(Error::Git {
            args: display_args(args),
            code: raw.code,
            stderr: raw.stderr_string(),
        })
    }
}
