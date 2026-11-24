//! Running the Inno Setup installer.

use std::path::Path;
use std::process::Command;

/// The documented silent update (docs/adr/0002-instalador.md): no UI, no message boxes, never
/// reboot, close the running Selachi and start it again afterwards.
pub const INSTALLER_ARGS: &[&str] = &[
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/SP-",
    "/CLOSEAPPLICATIONS",
    "/RESTARTAPPLICATIONS",
];

/// The same, for "install when I close Selachi": the app is already gone (or going), so it is not
/// started again.
pub const ON_EXIT_ARGS: &[&str] = &[
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/SP-",
    "/CLOSEAPPLICATIONS",
];

/// The command that would run, without spawning it (what [`launch`] does and what the tests check).
pub fn installer_command(installer: &Path, restart: bool) -> Command {
    let mut cmd = Command::new(installer);
    cmd.args(if restart {
        INSTALLER_ARGS
    } else {
        ON_EXIT_ARGS
    });
    cmd
}

/// Starts the installer detached from this process (it outlives it: it has to replace our exe).
pub fn launch(installer: &Path, restart: bool) -> std::io::Result<()> {
    let mut cmd = installer_command(installer, restart);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}
