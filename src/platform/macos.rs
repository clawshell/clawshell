use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

pub fn clawshell_chown_spec() -> &'static str {
    "clawshell:staff"
}

pub fn pid_file_abs_path() -> &'static str {
    "/var/run/clawshell.pid"
}

pub fn pid_file_vfs_rel_path() -> &'static str {
    "var/run/clawshell.pid"
}

pub fn autostart_service_path() -> &'static str {
    "/Library/LaunchDaemons/com.clawshell.daemon.plist"
}

pub fn autostart_service_content(exe_path: &Path, config_path: &Path) -> String {
    crate::onboard::generate_launchd_plist(exe_path, config_path)
}

pub fn create_system_user(name: &str) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let output = Command::new("dscl")
        .args([".", "-list", "/Users", "UniqueID"])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let used_uids: Vec<u32> = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().last()?.parse().ok())
        .collect();
    let uid = (400..500)
        .rev()
        .find(|u| !used_uids.contains(u))
        .ok_or("No available system UID in 400-499 range")?;

    let user_path = format!("/Users/{name}");
    let uid_str = uid.to_string();

    let dscl = |args: &[&str], desc: &str| -> Result<ExitStatus, Box<dyn std::error::Error>> {
        let status = Command::new("dscl").args(args).status()?;
        if !status.success() {
            eprintln!("Warning: failed to {desc} for '{name}'");
        }
        Ok(status)
    };

    dscl(&[".", "-create", &user_path], "create user record")?;
    dscl(
        &[".", "-create", &user_path, "UniqueID", &uid_str],
        "set UID",
    )?;
    dscl(
        &[".", "-create", &user_path, "PrimaryGroupID", "20"],
        "set GID",
    )?;
    dscl(
        &[".", "-create", &user_path, "UserShell", "/usr/bin/false"],
        "set shell",
    )?;
    dscl(
        &[".", "-create", &user_path, "RealName", "ClawShell Service"],
        "set real name",
    )?;
    let status = dscl(
        &[".", "-create", &user_path, "NFSHomeDirectory", "/var/empty"],
        "set home directory",
    )?;

    let _ = Command::new("dscl")
        .args([".", "-create", &user_path, "IsHidden", "1"])
        .status();

    Ok(status)
}

pub fn delete_system_user(name: &str) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    Ok(Command::new("dscl")
        .args([".", "-delete", &format!("/Users/{name}")])
        .status()?)
}

pub fn install_autostart_post_write(service_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _ = Command::new("launchctl")
        .args(["unload", service_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("chown")
        .args(["root:wheel", service_path])
        .status();
    let _ = Command::new("chmod").args(["0644", service_path]).status();
    Ok(())
}

pub fn start_autostart_service(service_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("launchctl")
        .args(["load", service_path])
        .status()?;
    if !status.success() {
        return Err(format!("launchctl load failed (exit code {})", status).into());
    }
    Ok(())
}

pub fn remove_autostart_service(service_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _ = Command::new("launchctl")
        .args(["unload", service_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

pub fn remove_autostart_post_delete() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
