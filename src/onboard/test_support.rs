use super::OnboardConfig;
use std::path::PathBuf;
use vfs::VfsPath;

pub(super) fn test_config() -> OnboardConfig {
    OnboardConfig {
        provider: "openai".to_string(),
        model: "gpt-5.2".to_string(),
        auth_method: super::types::OnboardAuthMethod::StaticKey,
        real_api_key: "sk-real-key-123".to_string(),
        virtual_api_key: "{clawshell-virtual-key-openai}".to_string(),
        openclaw_config_path: PathBuf::from("/tmp/test-openclaw.json"),
        server_host: "127.0.0.1".to_string(),
        server_port: 18790,
        email: None,
    }
}

/// Create a VFS helper that writes content to a path, creating parent dirs.
pub(super) fn vfs_write(root: &VfsPath, path: &str, content: &str) {
    let p = root.join(path).unwrap();
    p.parent().create_dir_all().unwrap();
    p.create_file()
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
}
