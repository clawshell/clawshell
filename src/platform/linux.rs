use super::{Error, command_output, command_status, ensure_success};
use std::path::Path;
use std::process::{Command, ExitStatus};

pub fn clawshell_chown_spec() -> &'static str {
    "clawshell:clawshell"
}

pub fn pid_file_abs_path() -> &'static str {
    "/run/clawshell/clawshell.pid"
}

pub fn pid_file_vfs_rel_path() -> &'static str {
    "run/clawshell/clawshell.pid"
}

pub fn autostart_service_path() -> &'static str {
    "/etc/systemd/system/clawshell.service"
}

pub fn autostart_service_content(exe_path: &Path, config_path: &Path) -> String {
    crate::onboard::generate_systemd_unit(exe_path, config_path)
}

pub fn create_system_user(name: &str) -> Result<ExitStatus, Error> {
    let mut command = Command::new("useradd");
    command.args([
        "--system",
        "--no-create-home",
        "--shell",
        "/usr/sbin/nologin",
        name,
    ]);
    command_status(&mut command, "useradd")
}

pub fn delete_system_user(name: &str) -> Result<ExitStatus, Error> {
    let mut command = Command::new("userdel");
    command.arg(name);
    command_status(&mut command, "userdel")
}

pub fn install_autostart_post_write(_service_path: &str) -> Result<(), Error> {
    let mut daemon_reload = Command::new("systemctl");
    daemon_reload.args(["daemon-reload"]);
    let output = command_output(&mut daemon_reload, "systemctl daemon-reload")?;
    ensure_success("systemctl daemon-reload", output)?;

    let mut enable = Command::new("systemctl");
    enable.args(["enable", "clawshell.service"]);
    let output = command_output(&mut enable, "systemctl enable clawshell.service")?;
    ensure_success("systemctl enable clawshell.service", output)?;

    Ok(())
}

pub fn start_autostart_service(_service_path: &str) -> Result<(), Error> {
    let mut start = Command::new("systemctl");
    start.args(["start", "clawshell.service"]);
    let output = command_output(&mut start, "systemctl start clawshell.service")?;
    ensure_success("systemctl start clawshell.service", output)?;
    Ok(())
}

pub fn remove_autostart_service(_service_path: &str) -> Result<(), Error> {
    let _ = Command::new("systemctl")
        .args(["disable", "clawshell.service"])
        .status();
    let _ = Command::new("systemctl")
        .args(["stop", "clawshell.service"])
        .status();
    Ok(())
}

pub fn remove_autostart_post_delete() -> Result<(), Error> {
    let _ = Command::new("systemctl").args(["daemon-reload"]).status();
    Ok(())
}
