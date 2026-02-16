use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use predicates::str::contains;

fn cmd() -> Command {
    cargo_bin_cmd!("clawshell")
}

#[cfg(target_os = "linux")]
fn pid_file_path() -> std::path::PathBuf {
    "/run/clawshell/clawshell.pid".into()
}

#[cfg(target_os = "macos")]
fn pid_file_path() -> std::path::PathBuf {
    "/var/run/clawshell.pid".into()
}

fn log_file_path() -> std::path::PathBuf {
    "/var/log/clawshell/clawshell.log".into()
}

/// Try to ensure the log directory exists so tests can write log files.
/// Returns true if we have write access.
fn ensure_log_dir() -> bool {
    let path = log_file_path();
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    // Check write access by trying to create/touch the file
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .is_ok()
}

#[test]
fn test_help_output() {
    cmd()
        .arg("help")
        .assert()
        .success()
        .stdout(contains("ClawShell"))
        .stdout(contains("start"))
        .stdout(contains("stop"))
        .stdout(contains("status"))
        .stdout(contains("restart"))
        .stdout(contains("logs"))
        .stdout(contains("config"))
        .stdout(contains("onboard"))
        .stdout(contains("version"));
}

#[test]
fn test_version_output() {
    cmd()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("clawshell").or(contains("ClawShell").or(contains("Clawshell"))))
        .stdout(contains("v0.0.1"))
        .stdout(contains("openclaw").or(contains("OpenClaw")));
}

#[test]
fn test_status_when_not_running() {
    let _ = std::fs::remove_file(pid_file_path());

    cmd()
        .arg("status")
        .assert()
        .success()
        .stdout(contains("not running"));
}

#[test]
fn test_stop_when_not_running() {
    let _ = std::fs::remove_file(pid_file_path());

    cmd()
        .arg("stop")
        .assert()
        .success()
        .stdout(contains("not running"));
}

#[test]
fn test_start_with_invalid_config() {
    cmd()
        .args([
            "start",
            "--config",
            "/nonexistent/config.toml",
            "--foreground",
        ])
        .assert()
        .failure();
}

#[test]
fn test_config_display_missing_file() {
    cmd()
        .args(["config", "--file", "/nonexistent/config.toml"])
        .assert()
        .failure();
}

#[test]
fn test_config_display_example_file() {
    cmd()
        .args(["config", "--file", "clawshell.example.toml"])
        .assert()
        .success()
        .stdout(contains("ClawShell"))
        .stdout(contains("Configuration"))
        .stdout(contains("Listen:"))
        .stdout(contains("configured"));
}

/// Combined log tests to avoid race conditions on the shared log file.
/// Skipped if we don't have write access to the log directory.
#[test]
fn test_logs_commands() {
    let log_path = log_file_path();

    if !ensure_log_dir() {
        eprintln!(
            "Skipping log tests: no write access to {}",
            log_path.parent().unwrap().display()
        );
        return;
    }

    // Test 1: No log file
    let _ = std::fs::remove_file(&log_path);
    cmd()
        .arg("logs")
        .assert()
        .success()
        .stdout(contains("No logs available"));

    // Test 2: Level filter
    std::fs::write(
        &log_path,
        "2024-01-01 INFO Starting server\n2024-01-01 ERROR Something failed\n2024-01-01 DEBUG Debug message\n",
    )
    .unwrap();

    cmd()
        .args(["logs", "--level", "error"])
        .assert()
        .success()
        .stdout(contains("ERROR"))
        .stdout(contains("INFO Starting").not())
        .stdout(contains("DEBUG").not());

    // Test 3: Keyword filter
    std::fs::write(
        &log_path,
        "2024-01-01 INFO Starting server\n2024-01-01 INFO Request timeout\n2024-01-01 INFO Request completed\n",
    )
    .unwrap();

    cmd()
        .args(["logs", "--filter", "timeout"])
        .assert()
        .success()
        .stdout(contains("timeout"))
        .stdout(contains("Starting").not());

    // Test 4: Num limit
    let lines: String = (1..=20)
        .map(|i| format!("2024-01-01 INFO Line {}\n", i))
        .collect();
    std::fs::write(&log_path, &lines).unwrap();

    cmd()
        .args(["logs", "--num", "5"])
        .assert()
        .success()
        .stdout(contains("Line 16"))
        .stdout(contains("Line 20"));

    // Clean up
    let _ = std::fs::remove_file(&log_path);
}

#[test]
fn test_help_subcommand_examples() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("EXAMPLES"))
        .stdout(contains("clawshell start"))
        .stdout(contains("clawshell stop"));
}

#[test]
fn test_onboard_requires_root() {
    if !nix::unistd::getuid().is_root() {
        cmd()
            .arg("onboard")
            .assert()
            .failure()
            .stdout(contains("Administrative Privileges Required"))
            .stdout(contains("sudo clawshell onboard"));
    }
}

#[test]
fn test_uninstall_requires_root() {
    if !nix::unistd::getuid().is_root() {
        cmd()
            .args(["uninstall", "--yes"])
            .assert()
            .failure()
            .stdout(contains("Administrative Privileges Required"))
            .stdout(contains("sudo clawshell uninstall"));
    }
}

#[test]
fn test_help_shows_uninstall() {
    cmd()
        .arg("help")
        .assert()
        .success()
        .stdout(contains("uninstall"));
}
