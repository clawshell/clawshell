use nix::sys::signal::{self, Signal};
use nix::unistd::{Gid, Pid, Uid, User, getuid, setgid, setuid};
use nix::unistd::{SysconfVar, sysconf};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

/// Default configuration directory.
pub const CONFIG_DIR: &str = "/etc/clawshell";

/// Default configuration file path.
pub fn default_config_path() -> PathBuf {
    PathBuf::from(CONFIG_DIR).join("clawshell.toml")
}

/// PID file location.
/// - Linux: /run/clawshell/clawshell.pid
/// - macOS: /var/run/clawshell.pid (flat, no subdirectory since /var/run is a symlink to /private/var/run)
pub fn pid_file_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/var/run/clawshell.pid")
    } else {
        PathBuf::from("/run/clawshell/clawshell.pid")
    }
}

/// Log file location.
/// - Linux: /var/log/clawshell/clawshell.log
/// - macOS: /var/log/clawshell/clawshell.log
pub fn log_file_path() -> PathBuf {
    PathBuf::from("/var/log/clawshell/clawshell.log")
}

/// Ensure the parent directories for PID and log files exist.
pub fn ensure_runtime_dirs() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = pid_file_path().parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = log_file_path().parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn write_pid_file(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(pid_file_path(), pid.to_string())?;
    Ok(())
}

pub fn read_pid_file() -> Option<u32> {
    fs::read_to_string(pid_file_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

pub fn remove_pid_file() {
    let _ = fs::remove_file(pid_file_path());
}

fn to_pid(pid: u32) -> Result<Pid, Box<dyn std::error::Error>> {
    let raw: i32 = pid
        .try_into()
        .map_err(|_| format!("PID {} exceeds i32::MAX", pid))?;
    Ok(Pid::from_raw(raw))
}

pub fn is_process_running(pid: u32) -> bool {
    to_pid(pid)
        .map(|p| signal::kill(p, None).is_ok())
        .unwrap_or(false)
}

pub fn stop_process(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    let nix_pid = to_pid(pid)?;
    signal::kill(nix_pid, Signal::SIGTERM)
        .map_err(|e| format!("Failed to send SIGTERM to process {}: {}", pid, e))?;

    // Wait for the process to exit (up to 10 seconds)
    for _ in 0..100 {
        if !is_process_running(pid) {
            remove_pid_file();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Force kill if still running
    eprintln!(
        "Process {} did not stop gracefully, sending SIGKILL...",
        pid
    );
    signal::kill(nix_pid, Signal::SIGKILL)
        .map_err(|e| format!("Failed to send SIGKILL to process {}: {}", pid, e))?;
    remove_pid_file();
    Ok(())
}

/// Drop privileges from root to the `clawshell` system user.
///
/// This resolves the `clawshell` user, then calls `setgid` followed by `setuid`
/// so the process runs with minimal privileges. Only acts when running as root;
/// returns `Ok(())` immediately otherwise.
pub fn drop_privileges() -> Result<(), Box<dyn std::error::Error>> {
    if !getuid().is_root() {
        return Ok(());
    }

    let user = User::from_name("clawshell")?
        .ok_or("system user 'clawshell' not found — run `sudo clawshell onboard` first")?;

    setgid(Gid::from_raw(user.gid.as_raw())).map_err(|e| format!("setgid({}): {}", user.gid, e))?;
    setuid(Uid::from_raw(user.uid.as_raw())).map_err(|e| format!("setuid({}): {}", user.uid, e))?;

    info!(
        uid = user.uid.as_raw(),
        gid = user.gid.as_raw(),
        "Dropped privileges to 'clawshell'"
    );
    Ok(())
}

pub fn get_process_uptime(pid: u32) -> Option<String> {
    let stat_path = format!("/proc/{}/stat", pid);
    let stat = fs::read_to_string(&stat_path).ok()?;
    let boot_time = get_boot_time()?;
    let fields: Vec<&str> = stat.split_whitespace().collect();
    // Field 21 (0-indexed) is starttime in clock ticks
    let start_ticks: u64 = fields.get(21)?.parse().ok()?;
    let ticks_per_sec: u64 = sysconf(SysconfVar::CLK_TCK).ok()?? as u64;
    let start_secs = boot_time + start_ticks / ticks_per_sec;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let uptime_secs = now.saturating_sub(start_secs);
    Some(format_duration(uptime_secs))
}

fn get_boot_time() -> Option<u64> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    for line in stat.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, minutes, seconds)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process;

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(42), "42s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(125), "2m 5s");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3661), "1h 1m 1s");
    }

    #[test]
    fn test_format_duration_days() {
        assert_eq!(format_duration(90061), "1d 1h 1m 1s");
    }

    #[test]
    fn test_pid_file_path() {
        let path = pid_file_path();
        let path_str = path.to_str().unwrap();
        assert!(path_str.contains("clawshell.pid"));
        // Should be under /run or /var/run, not /tmp
        assert!(
            path_str.starts_with("/run/") || path_str.starts_with("/var/run"),
            "PID path should be under /run or /var/run, got: {}",
            path_str
        );
    }

    #[test]
    fn test_log_file_path() {
        let path = log_file_path();
        let path_str = path.to_str().unwrap();
        assert!(path_str.contains("clawshell.log"));
        assert!(
            path_str.starts_with("/var/log/"),
            "Log path should be under /var/log, got: {}",
            path_str
        );
    }

    #[test]
    fn test_default_config_path() {
        let path = default_config_path();
        let path_str = path.to_str().unwrap();
        assert_eq!(path_str, "/etc/clawshell/clawshell.toml");
    }

    #[test]
    fn test_write_and_read_pid_file() {
        let pid_path = pid_file_path();
        // Ensure the PID directory exists and is writable
        if let Some(parent) = pid_path.parent() {
            if fs::create_dir_all(parent).is_err() {
                eprintln!("Skipping test_write_and_read_pid_file: cannot create PID dir");
                return;
            }
            // Check we can actually write in this directory
            let probe = parent.join(".clawshell_write_probe");
            if fs::write(&probe, b"").is_err() {
                eprintln!("Skipping test_write_and_read_pid_file: PID dir not writable");
                return;
            }
            let _ = fs::remove_file(&probe);
        }
        let test_pid = process::id();
        write_pid_file(test_pid).unwrap();
        let read_pid = read_pid_file().unwrap();
        assert_eq!(read_pid, test_pid);
        remove_pid_file();
        assert!(read_pid_file().is_none());
    }

    #[test]
    fn test_is_process_running_self() {
        let pid = process::id();
        assert!(is_process_running(pid));
    }

    #[test]
    fn test_is_process_running_nonexistent() {
        // PID 99999999 should not exist
        assert!(!is_process_running(99999999));
    }

    #[test]
    fn test_ensure_runtime_dirs() {
        // This may fail without root, but should not panic
        let result = ensure_runtime_dirs();
        // If we have permissions it succeeds; if not, it returns an error
        if result.is_ok() {
            assert!(pid_file_path().parent().unwrap().exists());
            assert!(log_file_path().parent().unwrap().exists());
        }
    }

    #[test]
    fn test_get_process_uptime_self() {
        let pid = process::id();
        // On Linux with /proc, this should return Some
        if Path::new("/proc/self/stat").exists() {
            let uptime = get_process_uptime(pid);
            assert!(uptime.is_some(), "Should be able to get uptime for self");
            let uptime_str = uptime.unwrap();
            // Should be a valid duration string (ends with 's')
            assert!(
                uptime_str.ends_with('s'),
                "Uptime should end with 's': {}",
                uptime_str
            );
        }
    }

    #[test]
    fn test_get_process_uptime_nonexistent() {
        let uptime = get_process_uptime(99999999);
        assert!(uptime.is_none());
    }

    #[test]
    fn test_get_boot_time() {
        if Path::new("/proc/stat").exists() {
            let boot_time = get_boot_time();
            assert!(
                boot_time.is_some(),
                "Should be able to read boot time from /proc/stat"
            );
            assert!(boot_time.unwrap() > 0);
        }
    }

    #[test]
    fn test_stop_process_nonexistent() {
        // Stopping a nonexistent process should fail with an error
        let result = stop_process(99999999);
        assert!(result.is_err());
    }

    #[test]
    fn test_stop_process_spawned_child() {
        // Spawn a sleep process and then stop it
        let child = process::Command::new("sleep").arg("60").spawn();
        if let Ok(mut child) = child {
            let pid = child.id();
            assert!(is_process_running(pid));
            let result = stop_process(pid);
            assert!(result.is_ok());
            // Wait for the child to be reaped
            let _ = child.wait();
        }
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(0), "0s");
    }

    #[test]
    fn test_format_duration_exactly_one_hour() {
        assert_eq!(format_duration(3600), "1h 0m 0s");
    }

    #[test]
    fn test_format_duration_exactly_one_day() {
        assert_eq!(format_duration(86400), "1d 0h 0m 0s");
    }

    #[test]
    fn test_to_pid_valid() {
        let pid = to_pid(1234).unwrap();
        assert_eq!(pid.as_raw(), 1234);
    }

    #[test]
    fn test_to_pid_max_i32() {
        let pid = to_pid(i32::MAX as u32).unwrap();
        assert_eq!(pid.as_raw(), i32::MAX);
    }

    #[test]
    fn test_to_pid_overflow() {
        let result = to_pid(u32::MAX);
        assert!(result.is_err());
    }

    #[test]
    fn test_drop_privileges_no_clawshell_user() {
        // On non-root CI environments without a clawshell user,
        // drop_privileges should return Ok (since getuid is not root).
        if getuid().is_root() {
            // When running as root without the user, it should error
            if User::from_name("clawshell").ok().flatten().is_none() {
                let result = drop_privileges();
                assert!(
                    result.is_err(),
                    "Should fail when clawshell user doesn't exist"
                );
                assert!(
                    result.unwrap_err().to_string().contains("not found"),
                    "Error should mention user not found"
                );
            }
        } else {
            // Not root — should be a no-op success
            let result = drop_privileges();
            assert!(result.is_ok(), "Should succeed as no-op when not root");
        }
    }

    #[test]
    fn test_drop_privileges_as_root() {
        // Skip unless running as root with the clawshell user present
        if !getuid().is_root() {
            eprintln!("Skipping test_drop_privileges_as_root: not running as root");
            return;
        }
        if User::from_name("clawshell").ok().flatten().is_none() {
            eprintln!("Skipping test_drop_privileges_as_root: clawshell user not found");
            return;
        }
        // NOTE: actually calling drop_privileges() here would permanently
        // change the process UID/GID, affecting other tests. We verify the
        // user lookup works; the real integration is tested manually.
        let user = User::from_name("clawshell").unwrap().unwrap();
        assert!(user.uid.as_raw() > 0, "clawshell should not be UID 0");
    }
}
