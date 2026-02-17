use super::{Error, command_output, command_status, format_octal_mode};
use std::path::{Path, PathBuf};
use std::process::Command;

const LAUNCHD_LABEL: &str = "system/com.clawshell.daemon";

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

pub fn generate_launchd_plist(exe_path: &Path, config_path: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.clawshell.daemon</string>
    <key>UserName</key>
    <string>clawshell</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>start</string>
        <string>--config</string>
        <string>{config}</string>
        <string>--foreground</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/var/log/clawshell/clawshell.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/clawshell/clawshell.log</string>
</dict>
</plist>
"#,
        exe = exe_path.display(),
        config = config_path.display(),
    )
}

pub fn autostart_service_content(exe_path: &Path, config_path: &Path) -> String {
    generate_launchd_plist(exe_path, config_path)
}

pub fn create_system_user(name: &str) -> Result<(), Error> {
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

    let dscl = |args: &[&str], command_name: &'static str| -> Result<(), Error> {
        let mut command = Command::new("dscl");
        command.args(args);
        command_status(&mut command, command_name)
    };

    dscl(&[".", "-create", &user_path], "dscl create user record")?;
    dscl(
        &[".", "-create", &user_path, "UniqueID", &uid_str],
        "dscl set user UID",
    )?;
    dscl(
        &[".", "-create", &user_path, "PrimaryGroupID", "20"],
        "dscl set user GID",
    )?;
    dscl(
        &[".", "-create", &user_path, "UserShell", "/usr/bin/false"],
        "dscl set user shell",
    )?;
    dscl(
        &[".", "-create", &user_path, "RealName", "ClawShell Service"],
        "dscl set user real name",
    )?;
    dscl(
        &[".", "-create", &user_path, "NFSHomeDirectory", "/var/empty"],
        "dscl set user home directory",
    )?;

    let mut hide_user = Command::new("dscl");
    hide_user.args([".", "-create", &user_path, "IsHidden", "1"]);
    command_status(&mut hide_user, "dscl hide user")?;

    Ok(())
}

pub fn delete_system_user(name: &str) -> Result<(), Error> {
    let mut command = Command::new("dscl");
    command.args([".", "-delete", &format!("/Users/{name}")]);
    command_status(&mut command, "dscl -delete /Users")
}

pub fn install_autostart_post_write(service_path: &str) -> Result<(), Error> {
    let mut chown = Command::new("chown");
    chown.args(["root:wheel", service_path]);
    command_status(&mut chown, "chown")?;

    let mut chmod = Command::new("chmod");
    chmod.args(["0644", service_path]);
    command_status(&mut chmod, "chmod")?;

    Ok(())
}

pub fn start_autostart_service(service_path: &str) -> Result<(), Error> {
    let _ = service_path;
    service_start()
}

pub fn service_exists() -> Result<bool, Error> {
    Ok(Path::new(autostart_service_path()).exists())
}

pub fn service_start() -> Result<(), Error> {
    let mut kickstart = Command::new("launchctl");
    kickstart.args(["kickstart", "-k", LAUNCHD_LABEL]);
    if command_status(&mut kickstart, "launchctl kickstart").is_ok() {
        return Ok(());
    }

    let mut load = Command::new("launchctl");
    load.args(["load", autostart_service_path()]);
    command_status(&mut load, "launchctl load")?;
    Ok(())
}

pub fn service_stop() -> Result<(), Error> {
    let mut bootout = Command::new("launchctl");
    bootout.args(["bootout", LAUNCHD_LABEL]);
    if command_status(&mut bootout, "launchctl bootout").is_ok() {
        return Ok(());
    }

    let mut unload = Command::new("launchctl");
    unload.args(["unload", autostart_service_path()]);
    command_status(&mut unload, "launchctl unload")?;
    Ok(())
}

pub fn service_restart() -> Result<(), Error> {
    service_stop()?;
    service_start()
}

pub fn service_is_running() -> Result<bool, Error> {
    let mut print = Command::new("launchctl");
    print.args(["print", LAUNCHD_LABEL]);
    let output = command_output(&mut print, "launchctl print")?;
    Ok(output.status.success())
}

pub fn service_config_path() -> Result<Option<PathBuf>, Error> {
    let service_path = PathBuf::from(autostart_service_path());
    if !service_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&service_path).map_err(|source| Error::FileIo {
        operation: "read service file",
        path: service_path,
        source,
    })?;

    Ok(parse_launchd_config_path(&content))
}

fn parse_launchd_config_path(content: &str) -> Option<PathBuf> {
    let mut in_program_arguments = false;
    let mut saw_program_arguments_key = false;
    let mut args = Vec::<String>::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "<key>ProgramArguments</key>" {
            saw_program_arguments_key = true;
            continue;
        }

        if saw_program_arguments_key && trimmed == "<array>" {
            in_program_arguments = true;
            saw_program_arguments_key = false;
            continue;
        }

        if in_program_arguments && trimmed == "</array>" {
            break;
        }

        if in_program_arguments
            && let Some(value) = trimmed
                .strip_prefix("<string>")
                .and_then(|v| v.strip_suffix("</string>"))
        {
            args.push(value.to_string());
        }
    }

    for (idx, arg) in args.iter().enumerate() {
        if arg == "--config" {
            return args.get(idx + 1).map(PathBuf::from);
        }
    }

    None
}

pub fn remove_autostart_service(service_path: &str) -> Result<(), Error> {
    let _ = service_path;
    service_stop()
}

pub fn remove_autostart_post_delete() -> Result<(), Error> {
    Ok(())
}

pub fn set_owner(path: &Path, recursive: bool) -> Result<(), Error> {
    let mut command = Command::new("chown");
    if recursive {
        command.arg("-R");
    }
    let path_arg = path.to_string_lossy().into_owned();
    command.args([clawshell_chown_spec(), path_arg.as_str()]);
    let op = if recursive { "chown -R" } else { "chown" };
    command_status(&mut command, op)
}

pub fn set_mode(path: &Path, mode_bits: u32) -> Result<(), Error> {
    let mode_str = format_octal_mode(mode_bits);
    let path_arg = path.to_string_lossy().into_owned();
    let mut command = Command::new("chmod");
    command.args([mode_str.as_str(), path_arg.as_str()]);
    command_status(&mut command, "chmod")
}

#[cfg(test)]
mod tests {
    use super::{generate_launchd_plist, parse_launchd_config_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn test_generate_launchd_plist_contains_required_fields() {
        let content = generate_launchd_plist(
            Path::new("/usr/local/bin/clawshell"),
            Path::new("/etc/clawshell/clawshell.toml"),
        );
        assert!(content.contains("<string>com.clawshell.daemon</string>"));
        assert!(content.contains("<string>clawshell</string>"));
        assert!(content.contains("<key>KeepAlive</key>"));
        assert!(content.contains("<true/>"));
        assert!(content.contains("<key>RunAtLoad</key>"));
        assert!(content.contains("<string>/usr/local/bin/clawshell</string>"));
        assert!(content.contains("<string>/var/log/clawshell/clawshell.log</string>"));
        assert!(content.contains("<key>ProgramArguments</key>"));
    }

    #[test]
    fn test_generate_launchd_plist_custom_paths() {
        let content = generate_launchd_plist(
            Path::new("/opt/cs/bin/clawshell"),
            Path::new("/opt/cs/config.toml"),
        );
        assert!(content.contains("<string>/opt/cs/bin/clawshell</string>"));
        assert!(content.contains("<string>/opt/cs/config.toml</string>"));
    }

    #[test]
    fn test_generate_launchd_plist_valid_xml_structure() {
        let content = generate_launchd_plist(
            Path::new("/usr/local/bin/clawshell"),
            Path::new("/etc/clawshell/clawshell.toml"),
        );
        assert!(content.starts_with("<?xml version=\"1.0\""));
        assert!(content.contains("<!DOCTYPE plist"));
        assert!(content.contains("</plist>"));
    }

    #[test]
    fn test_parse_launchd_config_path_present() {
        let content = generate_launchd_plist(
            Path::new("/usr/local/bin/clawshell"),
            Path::new("/etc/clawshell/clawshell.toml"),
        );
        let found = parse_launchd_config_path(&content);
        assert_eq!(found, Some(PathBuf::from("/etc/clawshell/clawshell.toml")));
    }

    #[test]
    fn test_parse_launchd_config_path_missing() {
        let content = r#"
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.clawshell.daemon</string>
</dict>
</plist>
"#;
        let found = parse_launchd_config_path(content);
        assert!(found.is_none());
    }
}
