use crate::platform;

use std::path::{Path, PathBuf};
use tracing::warn;
use vfs::VfsPath;

/// Core backup logic (VFS variant) — copies the file and handles numbered backups.
/// Does NOT apply Unix permissions or chown (MemoryFS doesn't support those).
pub(crate) fn backup_openclaw_config_vfs(
    openclaw_path: &VfsPath,
) -> Result<VfsPath, Box<dyn std::error::Error>> {
    if !openclaw_path.exists()? {
        return Err(format!(
            "OpenClaw configuration file not found at: {}",
            openclaw_path.as_str()
        )
        .into());
    }

    let parent = openclaw_path.parent();
    let base_backup = parent.join("openclaw.json.clawshell.bak")?;
    let backup_path = if base_backup.exists()? {
        // Find the next available numbered backup
        let mut n = 1u32;
        loop {
            let numbered = parent.join(format!("openclaw.json.clawshell.bak.{n}"))?;
            if !numbered.exists()? {
                break numbered;
            }
            n += 1;
        }
    } else {
        base_backup
    };

    let content = openclaw_path.read_to_string()?;
    backup_path.create_file()?.write_all(content.as_bytes())?;

    Ok(backup_path)
}

/// Backup the OpenClaw configuration file.
/// Returns the backup path on success.
pub fn backup_openclaw_config(openclaw_path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = crate::process::physical_root();
    let vfs_path = root.join(openclaw_path.to_string_lossy().trim_start_matches('/'))?;
    let backup_vfs = backup_openclaw_config_vfs(&vfs_path)?;
    let backup_path = PathBuf::from(backup_vfs.as_str());

    // Lock down the backup so no user can read it (contains sensitive config).
    // Restore requires `sudo chmod 600` first.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o000))?;

    // Chown the backup to the clawshell user.
    if let Err(error) = platform::set_owner(&backup_path, false) {
        warn!(
            error = %error,
            path = %backup_path.display(),
            "Failed to set backup owner"
        );
    }

    Ok(backup_path)
}

pub fn openclaw_config_root(openclaw_config_path: &Path) -> PathBuf {
    openclaw_config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vfs::{MemoryFS, VfsPath};

    #[test]
    fn test_openclaw_config_root_from_file_path() {
        let path = PathBuf::from("/home/user/.openclaw/openclaw.json");
        assert_eq!(
            openclaw_config_root(&path),
            PathBuf::from("/home/user/.openclaw")
        );
    }

    #[test]
    fn test_backup_openclaw_config() {
        let root = VfsPath::new(MemoryFS::new());
        let config_path = root.join("home/user/openclaw.json").unwrap();
        config_path.parent().create_dir_all().unwrap();
        config_path
            .create_file()
            .unwrap()
            .write_all(br#"{"test": true}"#)
            .unwrap();

        let backup_path = backup_openclaw_config_vfs(&config_path).unwrap();
        assert_eq!(
            backup_path.as_str(),
            "/home/user/openclaw.json.clawshell.bak"
        );
        assert!(backup_path.exists().unwrap());

        let backup_content = backup_path.read_to_string().unwrap();
        assert_eq!(backup_content, r#"{"test": true}"#);
    }

    #[test]
    fn test_backup_openclaw_config_numbered() {
        let root = VfsPath::new(MemoryFS::new());
        let config_path = root.join("home/user/openclaw.json").unwrap();
        config_path.parent().create_dir_all().unwrap();

        // First backup: creates .bak
        config_path
            .create_file()
            .unwrap()
            .write_all(br#"{"v": 0}"#)
            .unwrap();
        let bak0 = backup_openclaw_config_vfs(&config_path).unwrap();
        assert_eq!(bak0.as_str(), "/home/user/openclaw.json.clawshell.bak");

        // Second backup: .bak exists, creates .bak.1
        config_path
            .create_file()
            .unwrap()
            .write_all(br#"{"v": 1}"#)
            .unwrap();
        let bak1 = backup_openclaw_config_vfs(&config_path).unwrap();
        assert_eq!(bak1.as_str(), "/home/user/openclaw.json.clawshell.bak.1");

        // Third backup: .bak and .bak.1 exist, creates .bak.2
        config_path
            .create_file()
            .unwrap()
            .write_all(br#"{"v": 2}"#)
            .unwrap();
        let bak2 = backup_openclaw_config_vfs(&config_path).unwrap();
        assert_eq!(bak2.as_str(), "/home/user/openclaw.json.clawshell.bak.2");

        // Verify contents
        assert_eq!(bak0.read_to_string().unwrap(), r#"{"v": 0}"#);
        assert_eq!(bak1.read_to_string().unwrap(), r#"{"v": 1}"#);
        assert_eq!(bak2.read_to_string().unwrap(), r#"{"v": 2}"#);
    }

    #[test]
    fn test_backup_openclaw_config_missing_file() {
        let root = VfsPath::new(MemoryFS::new());
        let config_path = root.join("nonexistent/openclaw.json").unwrap();
        let result = backup_openclaw_config_vfs(&config_path);
        assert!(result.is_err());
    }
}
