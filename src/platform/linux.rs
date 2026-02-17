use super::{Error, command_output, command_status, format_octal_mode};
use std::path::{Path, PathBuf};
use std::process::Command;

const SERVICE_UNIT_NAME: &str = "clawshell.service";

pub fn clawshell_chown_spec() -> &'static str {
    "clawshell:clawshell"
}

pub fn autostart_service_path() -> &'static str {
    "/etc/systemd/system/clawshell.service"
}

pub fn generate_systemd_unit(exe_path: &Path, config_path: &Path) -> String {
    format!(
        r#"[Unit]
Description=ClawShell API proxy daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
User=clawshell
Group=clawshell
ExecStart={exe} start --config {config} --foreground
Restart=on-failure
RestartSec=5
StandardOutput=append:/var/log/clawshell/clawshell.log
StandardError=append:/var/log/clawshell/clawshell.log

[Install]
WantedBy=multi-user.target
"#,
        exe = exe_path.display(),
        config = config_path.display(),
    )
}

pub fn autostart_service_content(exe_path: &Path, config_path: &Path) -> String {
    generate_systemd_unit(exe_path, config_path)
}

pub fn create_system_user(name: &str) -> Result<(), Error> {
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

pub fn delete_system_user(name: &str) -> Result<(), Error> {
    let mut command = Command::new("userdel");
    command.arg(name);
    command_status(&mut command, "userdel")
}

pub fn install_autostart_post_write(_service_path: &str) -> Result<(), Error> {
    let mut daemon_reload = Command::new("systemctl");
    daemon_reload.args(["daemon-reload"]);
    command_status(&mut daemon_reload, "systemctl daemon-reload")?;

    let mut enable = Command::new("systemctl");
    enable.args(["enable", "clawshell.service"]);
    command_status(&mut enable, "systemctl enable clawshell.service")?;

    Ok(())
}

pub fn start_autostart_service(_service_path: &str) -> Result<(), Error> {
    service_start()
}

pub fn service_exists() -> Result<bool, Error> {
    Ok(Path::new(autostart_service_path()).exists())
}

pub fn service_start() -> Result<(), Error> {
    let mut start = Command::new("systemctl");
    start.args(["start", SERVICE_UNIT_NAME]);
    command_status(&mut start, "systemctl start clawshell.service")?;
    Ok(())
}

pub fn service_stop() -> Result<(), Error> {
    let mut stop = Command::new("systemctl");
    stop.args(["stop", SERVICE_UNIT_NAME]);
    command_status(&mut stop, "systemctl stop clawshell.service")?;
    Ok(())
}

pub fn service_restart() -> Result<(), Error> {
    let mut restart = Command::new("systemctl");
    restart.args(["restart", SERVICE_UNIT_NAME]);
    command_status(&mut restart, "systemctl restart clawshell.service")?;
    Ok(())
}

pub fn service_is_running() -> Result<bool, Error> {
    let mut is_active = Command::new("systemctl");
    is_active.args(["is-active", "--quiet", SERVICE_UNIT_NAME]);
    let output = command_output(&mut is_active, "systemctl is-active clawshell.service")?;
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
    Ok(parse_systemd_execstart_config_path(&content))
}

fn parse_systemd_execstart_config_path(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let trimmed = line.trim();
        let Some(exec_start) = trimmed.strip_prefix("ExecStart=") else {
            continue;
        };

        let args = exec_start.split_whitespace().collect::<Vec<_>>();
        for (idx, arg) in args.iter().enumerate() {
            if *arg == "--config" {
                return args.get(idx + 1).map(PathBuf::from);
            }
        }
    }

    None
}

pub fn remove_autostart_service(_service_path: &str) -> Result<(), Error> {
    let mut disable = Command::new("systemctl");
    disable.args(["disable", "clawshell.service"]);
    command_status(&mut disable, "systemctl disable clawshell.service")?;

    let mut stop = Command::new("systemctl");
    stop.args(["stop", SERVICE_UNIT_NAME]);
    command_status(&mut stop, "systemctl stop clawshell.service")?;

    Ok(())
}

pub fn remove_autostart_post_delete() -> Result<(), Error> {
    let mut daemon_reload = Command::new("systemctl");
    daemon_reload.args(["daemon-reload"]);
    command_status(&mut daemon_reload, "systemctl daemon-reload")?;
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
    use super::{generate_systemd_unit, parse_systemd_execstart_config_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn test_generate_systemd_unit_contains_required_fields() {
        let content = generate_systemd_unit(
            Path::new("/usr/local/bin/clawshell"),
            Path::new("/etc/clawshell/clawshell.toml"),
        );
        assert!(content.contains("Type=exec"));
        assert!(content.contains("User=clawshell"));
        assert!(content.contains("Group=clawshell"));
        assert!(content.contains("ExecStart=/usr/local/bin/clawshell start --config /etc/clawshell/clawshell.toml --foreground"));
        assert!(content.contains("Restart=on-failure"));
        assert!(content.contains("RestartSec=5"));
        assert!(content.contains("After=network-online.target"));
        assert!(content.contains("WantedBy=multi-user.target"));
        assert!(content.contains("StandardOutput=append:/var/log/clawshell/clawshell.log"));
        assert!(content.contains("StandardError=append:/var/log/clawshell/clawshell.log"));
    }

    #[test]
    fn test_generate_systemd_unit_custom_paths() {
        let content = generate_systemd_unit(
            Path::new("/opt/clawshell/bin/cs"),
            Path::new("/opt/clawshell/config.toml"),
        );
        assert!(content.contains(
            "ExecStart=/opt/clawshell/bin/cs start --config /opt/clawshell/config.toml --foreground"
        ));
    }

    #[test]
    fn test_parse_systemd_execstart_config_path_present() {
        let unit = r#"
[Service]
ExecStart=/usr/local/bin/clawshell start --config /etc/clawshell/clawshell.toml --foreground
"#;

        let found = parse_systemd_execstart_config_path(unit);
        assert_eq!(found, Some(PathBuf::from("/etc/clawshell/clawshell.toml")));
    }

    #[test]
    fn test_parse_systemd_execstart_config_path_missing() {
        let unit = r#"
[Service]
ExecStart=/usr/local/bin/clawshell start --foreground
"#;

        let found = parse_systemd_execstart_config_path(unit);
        assert!(found.is_none());
    }
}
