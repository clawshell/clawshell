use std::process::{Command, ExitStatus};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use self::linux::*;
#[cfg(target_os = "macos")]
pub use self::macos::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("unsupported target OS: ClawShell currently supports only Linux and macOS");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{command} failed to execute: {source}")]
    CommandIo {
        command: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{command} failed with exit status {status}; stdout: {stdout}; stderr: {stderr}")]
    CommandFailed {
        command: &'static str,
        status: ExitStatus,
        stdout: String,
        stderr: String,
    },
    #[error("no available system UID in 400-499 range")]
    NoAvailableSystemUid,
}

fn command_output(
    command: &mut Command,
    command_name: &'static str,
) -> Result<std::process::Output, Error> {
    command.output().map_err(|source| Error::CommandIo {
        command: command_name,
        source,
    })
}

fn command_status(command: &mut Command, command_name: &'static str) -> Result<(), Error> {
    let output = command_output(command, command_name)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CommandFailed {
            command: command_name,
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

fn format_octal_mode(mode_bits: u32) -> String {
    format!("{:04o}", mode_bits)
}
