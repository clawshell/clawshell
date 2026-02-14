use std::process::Command;

fn cargo_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["run", "--quiet", "--"]);
    cmd
}

fn pid_file_path() -> std::path::PathBuf {
    clawshell::process::pid_file_path()
}

fn log_file_path() -> std::path::PathBuf {
    clawshell::process::log_file_path()
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
    let output = cargo_bin().arg("help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ClawShell"));
    assert!(stdout.contains("start"));
    assert!(stdout.contains("stop"));
    assert!(stdout.contains("status"));
    assert!(stdout.contains("restart"));
    assert!(stdout.contains("logs"));
    assert!(stdout.contains("config"));
    assert!(stdout.contains("onboard"));
    assert!(stdout.contains("version"));
}

#[test]
fn test_version_output() {
    let output = cargo_bin().arg("version").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lower = stdout.to_lowercase();
    assert!(lower.contains("clawshell"));
    assert!(stdout.contains("v0.0.1"));
    assert!(lower.contains("openclaw"));
}

#[test]
fn test_status_when_not_running() {
    let _ = std::fs::remove_file(pid_file_path());

    let output = cargo_bin().arg("status").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("not running"));
}

#[test]
fn test_stop_when_not_running() {
    let _ = std::fs::remove_file(pid_file_path());

    let output = cargo_bin().arg("stop").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("not running"));
}

#[test]
fn test_start_with_invalid_config() {
    let output = cargo_bin()
        .args([
            "start",
            "--config",
            "/nonexistent/config.toml",
            "--foreground",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success() || stderr.contains("Failed to load configuration"));
}

#[test]
fn test_config_display_missing_file() {
    let output = cargo_bin()
        .args(["config", "--file", "/nonexistent/config.toml"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || !output.status.success());
}

#[test]
fn test_config_display_example_file() {
    let output = cargo_bin()
        .args(["config", "--file", "clawshell.example.toml"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ClawShell"));
    assert!(stdout.contains("Configuration"));
    assert!(stdout.contains("Listen:"));
    assert!(stdout.contains("configured"));
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
    let output = cargo_bin().arg("logs").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No logs available"));

    // Test 2: Level filter
    std::fs::write(
        &log_path,
        "2024-01-01 INFO Starting server\n2024-01-01 ERROR Something failed\n2024-01-01 DEBUG Debug message\n",
    )
    .unwrap();

    let output = cargo_bin()
        .args(["logs", "--level", "error"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ERROR"), "stdout was: {}", stdout);
    assert!(!stdout.contains("INFO Starting"));
    assert!(!stdout.contains("DEBUG"));

    // Test 3: Keyword filter
    std::fs::write(
        &log_path,
        "2024-01-01 INFO Starting server\n2024-01-01 INFO Request timeout\n2024-01-01 INFO Request completed\n",
    )
    .unwrap();

    let output = cargo_bin()
        .args(["logs", "--filter", "timeout"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("timeout"));
    assert!(!stdout.contains("Starting"));

    // Test 4: Num limit
    let lines: String = (1..=20)
        .map(|i| format!("2024-01-01 INFO Line {}\n", i))
        .collect();
    std::fs::write(&log_path, &lines).unwrap();

    let output = cargo_bin().args(["logs", "--num", "5"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Line 16"), "stdout was: {}", stdout);
    assert!(stdout.contains("Line 20"));

    // Clean up
    let _ = std::fs::remove_file(&log_path);
}

#[test]
fn test_help_subcommand_examples() {
    let output = cargo_bin().arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("EXAMPLES"));
    assert!(stdout.contains("clawshell start"));
    assert!(stdout.contains("clawshell stop"));
}

#[test]
fn test_onboard_requires_root() {
    if !nix::unistd::getuid().is_root() {
        let output = cargo_bin().arg("onboard").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lower = stdout.to_lowercase();
        assert!(lower.contains("administrative privileges"));
        assert!(stdout.contains("sudo clawshell onboard"));
    }
}

#[test]
fn test_uninstall_requires_root() {
    if !nix::unistd::getuid().is_root() {
        let output = cargo_bin().args(["uninstall", "--yes"]).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lower = stdout.to_lowercase();
        assert!(lower.contains("administrative privileges"));
        assert!(stdout.contains("sudo clawshell uninstall"));
    }
}

#[test]
fn test_help_shows_uninstall() {
    let output = cargo_bin().arg("help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("uninstall"));
}
