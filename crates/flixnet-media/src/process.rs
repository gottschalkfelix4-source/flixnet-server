//! Spawning helper processes safely.

use std::ffi::OsStr;

use tokio::process::Command;

/// A command that never opens a console window on Windows and is killed when its
/// handle is dropped (e.g. a request was cancelled).
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.kill_on_drop(true).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
