use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OpenclawCredentialCleanupSummary {
    pub dot_env_entries_removed: usize,
    pub auth_profile_files_updated: usize,
    pub auth_profile_entries_removed: usize,
    pub oauth_entries_removed: usize,
    pub backup_files_created: usize,
}

impl OpenclawCredentialCleanupSummary {
    pub fn has_changes(&self) -> bool {
        self.dot_env_entries_removed > 0
            || self.auth_profile_files_updated > 0
            || self.auth_profile_entries_removed > 0
            || self.oauth_entries_removed > 0
            || self.backup_files_created > 0
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OpenclawCredentialCleanupPreview {
    pub state_dir: PathBuf,
    pub state_dir_exists: bool,
    pub dot_env: Option<OpenclawFileRemovalPreview>,
    pub auth_profiles: Vec<OpenclawFileRemovalPreview>,
    pub oauth: Option<OpenclawFileRemovalPreview>,
}

impl OpenclawCredentialCleanupPreview {
    pub fn has_changes(&self) -> bool {
        self.dot_env.is_some() || !self.auth_profiles.is_empty() || self.oauth.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenclawFileRemovalPreview {
    pub path: PathBuf,
    pub backup_path: PathBuf,
    pub removals: Vec<String>,
}

/// Authentication method chosen during onboarding.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum OnboardAuthMethod {
    /// Static API key (the traditional approach).
    #[default]
    StaticKey,
    /// OAuth provider supplies access tokens at runtime.
    OAuth {
        /// Provider identifier, e.g. "codex".
        provider_id: String,
    },
}

/// Collected onboarding configuration from user prompts.
#[derive(Debug, Clone)]
pub struct OnboardConfig {
    pub provider: String,
    pub model: String,
    pub auth_method: OnboardAuthMethod,
    /// Set for `StaticKey`; empty for `OAuth`.
    pub real_api_key: String,
    pub virtual_api_key: String,
    pub openclaw_config_path: PathBuf,
    pub server_host: String,
    pub server_port: u16,
    pub email: Option<OnboardEmailConfig>,
}

/// Optional Email endpoint settings collected during onboarding.
#[derive(Debug, Clone)]
pub struct OnboardEmailConfig {
    pub mode: OnboardEmailMode,
    pub sender_rules: Vec<String>,
    pub account_virtual_key: String,
    pub email: String,
    pub app_password: String,
    pub imap_host: String,
    pub imap_port: u16,
}

#[derive(Debug, Clone)]
pub struct OnboardSkillFile {
    pub relative_path: &'static str,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct OnboardSkillBundle {
    pub name: &'static str,
    pub files: Vec<OnboardSkillFile>,
}

pub const OPENCLAW_EMAIL_MESSAGES_SKILL_NAME: &str = "get-email-messages";

/// Sender filtering mode for the Email endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardEmailMode {
    Allowlist,
    Denylist,
}

impl OnboardEmailMode {
    pub(crate) fn as_toml_value(self) -> &'static str {
        match self {
            OnboardEmailMode::Allowlist => "allowlist",
            OnboardEmailMode::Denylist => "denylist",
        }
    }
}
