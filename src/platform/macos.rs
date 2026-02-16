use super::{Error, command_output, command_status, ensure_success};
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

pub fn create_system_user(name: &str) -> Result<ExitStatus, Error> {
    let mut list_users = Command::new("dscl");
    list_users.args([".", "-list", "/Users", "UniqueID"]);
    let output = command_output(&mut list_users, "dscl -list /Users UniqueID")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let used_uids: Vec<u32> = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().last()?.parse().ok())
        .collect();
    let uid = (400..500)
        .rev()
        .find(|u| !used_uids.contains(u))
        .ok_or(Error::NoAvailableSystemUid)?;

    let user_path = format!("/Users/{name}");
    let uid_str = uid.to_string();

    let dscl = |args: &[&str], desc: &str| -> Result<ExitStatus, Error> {
        let mut command = Command::new("dscl");
        command.args(args);
        let status = command_status(&mut command, "dscl")?;
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

    let mut hide_user = Command::new("dscl");
    hide_user.args([".", "-create", &user_path, "IsHidden", "1"]);
    let _ = command_status(&mut hide_user, "dscl");

    Ok(status)
}

pub fn delete_system_user(name: &str) -> Result<ExitStatus, Error> {
    let mut command = Command::new("dscl");
    command.args([".", "-delete", &format!("/Users/{name}")]);
    command_status(&mut command, "dscl -delete /Users")
}

pub fn install_autostart_post_write(service_path: &str) -> Result<(), Error> {
    let mut unload = Command::new("launchctl");
    unload
        .args(["unload", service_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = command_status(&mut unload, "launchctl unload");

    let mut chown = Command::new("chown");
    chown.args(["root:wheel", service_path]);
    let _ = command_status(&mut chown, "chown");

    let mut chmod = Command::new("chmod");
    chmod.args(["0644", service_path]);
    let _ = command_status(&mut chmod, "chmod");

    Ok(())
}

pub fn start_autostart_service(service_path: &str) -> Result<(), Error> {
    let mut load = Command::new("launchctl");
    load.args(["load", service_path]);
    let output = command_output(&mut load, "launchctl load")?;
    ensure_success("launchctl load", output)?;
    Ok(())
}

pub fn remove_autostart_service(service_path: &str) -> Result<(), Error> {
    let mut unload = Command::new("launchctl");
    unload
        .args(["unload", service_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = command_status(&mut unload, "launchctl unload");
    Ok(())
}

pub fn remove_autostart_post_delete() -> Result<(), Error> {
    Ok(())
}
