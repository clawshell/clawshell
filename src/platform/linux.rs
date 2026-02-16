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

pub fn create_system_user(name: &str) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    Ok(Command::new("useradd")
        .args([
            "--system",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            name,
        ])
        .status()?)
}

pub fn delete_system_user(name: &str) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    Ok(Command::new("userdel").arg(name).status()?)
}

pub fn install_autostart_post_write(_service_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("systemctl").args(["daemon-reload"]).status()?;
    if !status.success() {
        return Err("systemctl daemon-reload failed".into());
    }

    let status = Command::new("systemctl")
        .args(["enable", "clawshell.service"])
        .status()?;
    if !status.success() {
        return Err("systemctl enable failed".into());
    }

    Ok(())
}

pub fn start_autostart_service(_service_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("systemctl")
        .args(["start", "clawshell.service"])
        .status()?;
    if !status.success() {
        return Err(format!("systemctl start failed (exit code {})", status).into());
    }
    Ok(())
}

pub fn remove_autostart_service(_service_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _ = Command::new("systemctl")
        .args(["disable", "clawshell.service"])
        .status();
    let _ = Command::new("systemctl")
        .args(["stop", "clawshell.service"])
        .status();
    Ok(())
}

pub fn remove_autostart_post_delete() -> Result<(), Box<dyn std::error::Error>> {
    let _ = Command::new("systemctl").args(["daemon-reload"]).status();
    Ok(())
}
