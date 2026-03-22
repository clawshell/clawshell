use super::OAuthTokens;
use std::path::PathBuf;
use tracing::debug;

/// Per-provider token persistence under a directory (e.g. `/etc/clawshell/oauth/`).
#[derive(Debug, Clone)]
pub struct TokenStorage {
    dir: PathBuf,
}

impl Default for TokenStorage {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("/etc/clawshell/oauth"),
        }
    }
}

impl TokenStorage {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    fn token_path(&self, provider_id: &str) -> PathBuf {
        self.dir.join(format!("{provider_id}.json"))
    }

    /// Save tokens for a provider, creating the directory if needed.
    pub fn save(&self, provider_id: &str, tokens: &OAuthTokens) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.token_path(provider_id);
        let content = serde_json::to_string_pretty(tokens)
            .map_err(|e| std::io::Error::other(format!("failed to serialize tokens: {e}")))?;
        std::fs::write(&path, content)?;

        // Set file permissions to 0600 (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }

        debug!(provider = %provider_id, path = %path.display(), "OAuth tokens saved");
        Ok(())
    }

    /// Load tokens for a provider, returning None if the file doesn't exist.
    pub fn load(&self, provider_id: &str) -> Result<Option<OAuthTokens>, std::io::Error> {
        let path = self.token_path(provider_id);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let tokens: OAuthTokens = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::other(format!("failed to parse tokens: {e}")))?;
        debug!(provider = %provider_id, path = %path.display(), "OAuth tokens loaded");
        Ok(Some(tokens))
    }

    /// Remove tokens for a provider.
    pub fn remove(&self, provider_id: &str) -> Result<(), std::io::Error> {
        let path = self.token_path(provider_id);
        if path.exists() {
            std::fs::remove_file(&path)?;
            debug!(provider = %provider_id, path = %path.display(), "OAuth tokens removed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn test_tokens() -> OAuthTokens {
        OAuthTokens {
            access_token: "access-123".to_string(),
            refresh_token: Some("refresh-456".to_string()),
            id_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            account_id: Some("user@test.com".to_string()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());

        let tokens = test_tokens();
        storage.save("test-provider", &tokens).unwrap();

        let loaded = storage.load("test-provider").unwrap().unwrap();
        assert_eq!(loaded.access_token, "access-123");
        assert_eq!(loaded.refresh_token.as_deref(), Some("refresh-456"));
        assert_eq!(loaded.account_id.as_deref(), Some("user@test.com"));
    }

    #[test]
    fn test_load_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());

        let loaded = storage.load("nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_remove() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());

        let tokens = test_tokens();
        storage.save("removable", &tokens).unwrap();
        assert!(storage.load("removable").unwrap().is_some());

        storage.remove("removable").unwrap();
        assert!(storage.load("removable").unwrap().is_none());
    }

    #[test]
    fn test_remove_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());
        // Should not error
        storage.remove("nonexistent").unwrap();
    }

    #[test]
    fn test_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        let storage = TokenStorage::new(nested.clone());

        let tokens = test_tokens();
        storage.save("test", &tokens).unwrap();
        assert!(nested.join("test.json").exists());
    }

    #[test]
    fn test_tokens_with_extra_fields() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());

        let mut tokens = test_tokens();
        tokens
            .extra
            .insert("project_id".to_string(), serde_json::json!("proj-abc-123"));
        tokens
            .extra
            .insert("tier".to_string(), serde_json::json!("production"));

        storage.save("extra-fields", &tokens).unwrap();

        let loaded = storage.load("extra-fields").unwrap().unwrap();
        assert_eq!(
            loaded.extra.get("project_id").unwrap().as_str().unwrap(),
            "proj-abc-123"
        );
        assert_eq!(
            loaded.extra.get("tier").unwrap().as_str().unwrap(),
            "production"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());

        let tokens = test_tokens();
        storage.save("perms-test", &tokens).unwrap();

        let path = dir.path().join("perms-test.json");
        let metadata = std::fs::metadata(path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
