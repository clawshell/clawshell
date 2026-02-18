use crate::platform;
use tracing::warn;
use vfs::VfsPath;

/// Return the platform-appropriate service file path.
pub fn autostart_service_path() -> &'static str {
    platform::autostart_service_path()
}

/// Write a service file to the given VFS path (testable with MemoryFS).
pub fn install_autostart_service_vfs(
    service_file: &VfsPath,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    service_file.parent().create_dir_all()?;
    service_file.create_file()?.write_all(content.as_bytes())?;
    Ok(())
}

/// Remove a service file from the given VFS path (testable with MemoryFS).
///
/// Returns `Ok(true)` if the file was removed, `Ok(false)` if it didn't exist.
pub fn remove_autostart_service_vfs(
    service_file: &VfsPath,
) -> Result<bool, Box<dyn std::error::Error>> {
    if service_file.exists()? {
        service_file.remove_file()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Install the auto-start service on the real filesystem and enable it.
pub fn install_autostart_service(
    exe_path: &std::path::Path,
    config_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let content = platform::autostart_service_content(exe_path, config_path);

    let service_path = autostart_service_path();
    let root = crate::process::physical_root();
    let vfs_path = root.join(service_path.trim_start_matches('/'))?;

    // Reinstall path: try to unload/disable first so replacing the unit is safe.
    // Whether this should be best-effort is a caller policy, not a platform policy.
    if vfs_path.exists()?
        && let Err(error) = platform::remove_autostart_service(service_path)
    {
        warn!(
            error = %error,
            service_path,
            "Failed to stop existing auto-start service before reinstall"
        );
    }

    install_autostart_service_vfs(&vfs_path, &content)?;
    platform::install_autostart_post_write(service_path)?;

    Ok(())
}

/// Start the auto-start service via the platform service manager.
pub fn start_autostart_service() -> Result<(), Box<dyn std::error::Error>> {
    let service_path = autostart_service_path();
    platform::start_autostart_service(service_path)?;
    Ok(())
}

/// Remove the auto-start service from the real filesystem and disable it.
pub fn remove_autostart_service() -> Result<(), Box<dyn std::error::Error>> {
    let service_path = autostart_service_path();
    platform::remove_autostart_service(service_path)?;

    let root = crate::process::physical_root();
    let vfs_path = root.join(service_path.trim_start_matches('/'))?;
    remove_autostart_service_vfs(&vfs_path)?;
    platform::remove_autostart_post_delete()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vfs::{MemoryFS, VfsPath};

    #[test]
    fn test_install_autostart_service_vfs_writes_file() {
        let root = VfsPath::new(MemoryFS::new());
        let service_file = root.join("etc/systemd/system/clawshell.service").unwrap();
        let content = "test service content";

        install_autostart_service_vfs(&service_file, content).unwrap();

        assert!(service_file.exists().unwrap());
        assert_eq!(service_file.read_to_string().unwrap(), content);
    }

    #[test]
    fn test_install_autostart_service_vfs_creates_parent_dirs() {
        let root = VfsPath::new(MemoryFS::new());
        let service_file = root
            .join("Library/LaunchDaemons/com.clawshell.daemon.plist")
            .unwrap();

        install_autostart_service_vfs(&service_file, "plist content").unwrap();

        assert!(service_file.exists().unwrap());
        assert!(
            root.join("Library/LaunchDaemons")
                .unwrap()
                .exists()
                .unwrap()
        );
    }

    #[test]
    fn test_install_autostart_service_vfs_overwrites_existing() {
        let root = VfsPath::new(MemoryFS::new());
        let service_file = root.join("etc/systemd/system/clawshell.service").unwrap();

        install_autostart_service_vfs(&service_file, "old content").unwrap();
        install_autostart_service_vfs(&service_file, "new content").unwrap();

        assert_eq!(service_file.read_to_string().unwrap(), "new content");
    }

    #[test]
    fn test_remove_autostart_service_vfs_removes_existing() {
        let root = VfsPath::new(MemoryFS::new());
        let service_file = root.join("etc/systemd/system/clawshell.service").unwrap();

        install_autostart_service_vfs(&service_file, "content").unwrap();
        assert!(service_file.exists().unwrap());

        let removed = remove_autostart_service_vfs(&service_file).unwrap();
        assert!(removed);
        assert!(!service_file.exists().unwrap());
    }

    #[test]
    fn test_remove_autostart_service_vfs_missing_file() {
        let root = VfsPath::new(MemoryFS::new());
        let service_file = root.join("etc/systemd/system/clawshell.service").unwrap();

        let removed = remove_autostart_service_vfs(&service_file).unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_autostart_service_path_is_absolute() {
        let path = autostart_service_path();
        assert!(path.starts_with('/'));
    }
}
