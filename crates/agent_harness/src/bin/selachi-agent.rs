//! `selachi-agent`: console binary of the agent harness (PLAN section 20, phase 1).
//!
//! The Selachi GUI executable is a Windows-subsystem program (no stdout), so the CLI is a separate
//! console binary for now. The integrator will later forward `selachi.exe agent ...` to it.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(agent_harness::cli::run_cli(&args));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("selachi-agent only runs on Windows");
    std::process::exit(2);
}
