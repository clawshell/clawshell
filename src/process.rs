use nix::unistd::{Gid, Uid, User, getuid, setgid, setuid};
use std::path::PathBuf;
use tracing::info;
use vfs::VfsPath;

/// Default configuration directory.
pub const CONFIG_DIR: &str = "/etc/clawshell";

/// Default configuration file path.
pub fn default_config_path() -> PathBuf {
    PathBuf::from(CONFIG_DIR).join("clawshell.toml")
}

/// Create a VFS root backed by the real filesystem.
pub(crate) fn physical_root() -> VfsPath {
    VfsPath::new(vfs::PhysicalFS::new("/"))
}

/// Log file path within a VFS root.
fn log_file_vfs(root: &VfsPath) -> Result<VfsPath, Box<dyn std::error::Error>> {
    Ok(root.join("var/log/clawshell/clawshell.log")?)
}

/// Log file location.
/// - Linux: /var/log/clawshell/clawshell.log
/// - macOS: /var/log/clawshell/clawshell.log
pub fn log_file_path() -> PathBuf {
    PathBuf::from("/var/log/clawshell/clawshell.log")
}

/// Ensure the parent directories for log files exist (VFS variant).
pub(crate) fn ensure_runtime_dirs_vfs(root: &VfsPath) -> Result<(), Box<dyn std::error::Error>> {
    let log_path = log_file_vfs(root)?;
    log_path.parent().create_dir_all()?;
    Ok(())
}

/// Ensure the parent directories for log files exist.
pub fn ensure_runtime_dirs() -> Result<(), Box<dyn std::error::Error>> {
    ensure_runtime_dirs_vfs(&physical_root())
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_ensure_runtime_dirs() {
        let root = VfsPath::new(vfs::MemoryFS::new());
        ensure_runtime_dirs_vfs(&root).unwrap();

        let log_parent = log_file_vfs(&root).unwrap().parent();
        assert!(log_parent.exists().unwrap());
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
