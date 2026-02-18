use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Openai,
    Openrouter,
    Anthropic,
}

impl Provider {
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Provider::Openai => "https://api.openai.com",
            Provider::Openrouter => "https://openrouter.ai/api",
            Provider::Anthropic => "https://api.anthropic.com",
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub server: ServerConfig,
    pub upstream: UpstreamConfig,
    #[serde(default)]
    pub keys: Vec<KeyMapping>,
    #[serde(default)]
    pub dlp: DlpConfig,
    #[serde(default, skip_serializing_if = "EmailConfig::is_default")]
    pub email: EmailConfig,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    18790
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    #[serde(default = "default_openai_base_url")]
    pub openai_base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter_base_url: Option<String>,
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    #[serde(default = "default_anthropic_version")]
    pub anthropic_version: String,
}

fn default_anthropic_version() -> String {
    "2023-06-01".to_string()
}

fn default_openai_base_url() -> String {
    "https://api.openai.com".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct KeyMapping {
    pub virtual_key: String,
    pub real_key: String,
    #[serde(default)]
    pub provider: Provider,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DlpAction {
    #[default]
    Block,
    Redact,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DlpConfig {
    #[serde(default)]
    pub patterns: Vec<DlpPattern>,
    #[serde(default = "default_scan_responses")]
    pub scan_responses: bool,
}

fn default_scan_responses() -> bool {
    true
}

impl Default for DlpConfig {
    fn default() -> Self {
        Self {
            patterns: Vec::new(),
            scan_responses: true,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DlpPattern {
    pub name: String,
    pub regex: String,
    #[serde(default)]
    pub action: DlpAction,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EmailConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<EmailMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_senders: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_senders: Vec<String>,
    #[serde(default = "default_email_max_results")]
    pub default_max_results: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<EmailAccountConfig>,
}

impl EmailConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: None,
            allow_senders: Vec::new(),
            deny_senders: Vec::new(),
            default_max_results: default_email_max_results(),
            accounts: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EmailMode {
    Allowlist,
    Denylist,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EmailAccountConfig {
    pub virtual_key: String,
    pub email: String,
    pub app_password: String,
    #[serde(default = "default_email_imap_host")]
    pub imap_host: String,
    #[serde(default = "default_email_imap_port")]
    pub imap_port: u16,
}

fn default_email_max_results() -> u32 {
    50
}

fn default_email_imap_host() -> String {
    "imap.gmail.com".to_string()
}

fn default_email_imap_port() -> u16 {
    993
}

impl Config {
    pub(crate) fn from_str_with_validation(
        content: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let config: Config = toml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Self::from_str_with_validation(&content)
    }

    #[cfg(test)]
    pub fn parse(content: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_str_with_validation(content)
    }

    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        for pattern in &self.dlp.patterns {
            Regex::new(&pattern.regex)
                .map_err(|e| format!("Invalid DLP regex for '{}': {}", pattern.name, e))?;
        }
        self.validate_email()?;
        Ok(())
    }

    fn validate_email(&self) -> Result<(), Box<dyn std::error::Error>> {
        let email = &self.email;

        if email.default_max_results == 0 || email.default_max_results > 100 {
            return Err(format!(
                "email.default_max_results must be between 1 and 100 (got {})",
                email.default_max_results
            )
            .into());
        }

        for sender in &email.allow_senders {
            validate_sender_rule(sender)
                .map_err(|e| format!("invalid allow_senders entry: {e}"))?;
        }
        for sender in &email.deny_senders {
            validate_sender_rule(sender).map_err(|e| format!("invalid deny_senders entry: {e}"))?;
        }

        let has_email_settings = email.enabled
            || email.mode.is_some()
            || !email.allow_senders.is_empty()
            || !email.deny_senders.is_empty()
            || !email.accounts.is_empty();

        if !has_email_settings {
            return Ok(());
        }

        let mode = email
            .mode
            .ok_or("email.mode is required when email settings are configured")?;

        match mode {
            EmailMode::Allowlist => {
                if email.allow_senders.is_empty() {
                    return Err(
                        "email.allow_senders must be non-empty when email.mode = \"allowlist\""
                            .into(),
                    );
                }
                if !email.deny_senders.is_empty() {
                    return Err(
                        "email.deny_senders must be empty when email.mode = \"allowlist\"".into(),
                    );
                }
            }
            EmailMode::Denylist => {
                if email.deny_senders.is_empty() {
                    return Err(
                        "email.deny_senders must be non-empty when email.mode = \"denylist\""
                            .into(),
                    );
                }
                if !email.allow_senders.is_empty() {
                    return Err(
                        "email.allow_senders must be empty when email.mode = \"denylist\"".into(),
                    );
                }
            }
        }

        if email.enabled && email.accounts.is_empty() {
            return Err("email.accounts must be non-empty when email.enabled = true".into());
        }

        for account in &email.accounts {
            if account.virtual_key.trim().is_empty() {
                return Err("email.accounts[].virtual_key must be non-empty".into());
            }

            let email = account.email.trim().to_ascii_lowercase();
            validate_email(&email)
                .map_err(|e| format!("email.accounts[].email is invalid: {e}"))?;

            if account.app_password.trim().is_empty() {
                return Err("email.accounts[].app_password must be non-empty".into());
            }

            if account.imap_host.trim().is_empty() {
                return Err("email.accounts[].imap_host must be non-empty".into());
            }

            if account.imap_port == 0 {
                return Err("email.accounts[].imap_port must be greater than 0".into());
            }
        }

        Ok(())
    }

    pub fn key_map(&self) -> BTreeMap<String, (String, Provider)> {
        self.keys
            .iter()
            .map(|k| (k.virtual_key.clone(), (k.real_key.clone(), k.provider)))
            .collect()
    }

    pub fn upstream_url(&self, provider: Provider) -> String {
        match provider {
            Provider::Openai => self.upstream.openai_base_url.clone(),
            Provider::Openrouter => self
                .upstream
                .openrouter_base_url
                .clone()
                .unwrap_or_else(|| Provider::Openrouter.default_base_url().to_string()),
            Provider::Anthropic => self
                .upstream
                .anthropic_base_url
                .clone()
                .unwrap_or_else(|| Provider::Anthropic.default_base_url().to_string()),
        }
    }

    pub fn listen_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }
}

pub(crate) fn validate_sender_rule(rule: &str) -> Result<(), String> {
    let rule = rule.trim().to_ascii_lowercase();
    if rule.is_empty() {
        return Err("sender rule cannot be empty".to_string());
    }
    if let Some(domain) = rule.strip_prefix('@') {
        validate_domain(domain).map_err(|e| format!("domain rule '{rule}' is invalid: {e}"))?;
        return Ok(());
    }
    validate_email(&rule).map_err(|e| format!("email rule '{rule}' is invalid: {e}"))
}

fn validate_email(email: &str) -> Result<(), String> {
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err("must contain exactly one '@'".to_string());
    }
    if local.is_empty() {
        return Err("local part cannot be empty".to_string());
    }
    if !local.chars().all(is_valid_email_local_char) {
        return Err("local part contains invalid characters".to_string());
    }
    validate_domain(domain)?;
    Ok(())
}

fn validate_domain(domain: &str) -> Result<(), String> {
    if domain.is_empty() {
        return Err("domain cannot be empty".to_string());
    }
    if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
        return Err("domain has invalid dot placement".to_string());
    }
    if !domain.contains('.') {
        return Err("domain must contain at least one dot".to_string());
    }
    if !domain
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
    {
        return Err("domain contains invalid characters".to_string());
    }
    Ok(())
}

fn is_valid_email_local_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '.' | '!'
                | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '/'
                | '='
                | '?'
                | '^'
                | '_'
                | '`'
                | '{'
                | '|'
                | '}'
                | '~'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::path::{Path, PathBuf};

    #[derive(Serialize)]
    struct ConfigSnapshot {
        #[serde(flatten)]
        config: Config,
        derived: DerivedValues,
    }

    #[derive(Serialize)]
    struct DerivedValues {
        listen_addr: String,
        openai_upstream_url: String,
        anthropic_upstream_url: String,
        key_map: BTreeMap<String, (String, Provider)>,
    }

    fn fixture_paths(root: &str) -> Result<Vec<PathBuf>, String> {
        let mut paths = std::fs::read_dir(root)
            .map_err(|e| format!("failed to read fixture directory '{root}': {e}"))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|e| format!("failed to read fixture entry in '{root}': {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        paths.retain(|path| path.extension().is_some_and(|ext| ext == "toml"));
        paths.sort();
        Ok(paths)
    }

    fn snapshot_name(path: &Path) -> Result<String, String> {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("invalid fixture file name: {}", path.display()))?;
        Ok(format!("config_fixtures__{stem}"))
    }

    fn assert_valid_config(path: &Path) -> Result<(), String> {
        let config = Config::from_file(path)
            .map_err(|e| format!("expected valid config {}: {e}", path.display()))?;
        let snapshot = ConfigSnapshot {
            derived: DerivedValues {
                listen_addr: config.listen_addr(),
                openai_upstream_url: config.upstream_url(Provider::Openai),
                anthropic_upstream_url: config.upstream_url(Provider::Anthropic),
                key_map: config.key_map(),
            },
            config,
        };
        let name = snapshot_name(path)?;
        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_yaml_snapshot!(name, snapshot);
        });
        Ok(())
    }

    fn assert_invalid_config(path: &Path) -> Result<(), String> {
        let err = Config::from_file(path).expect_err(&format!(
            "expected invalid config to fail: {}",
            path.display()
        ));
        let name = snapshot_name(path)?;
        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(name, err.to_string());
        });
        Ok(())
    }

    #[test]
    fn test_valid_config_fixtures() {
        let paths =
            fixture_paths("tests/fixtures/config/valid").expect("failed to load valid fixtures");
        assert!(!paths.is_empty(), "no valid fixtures found");
        for path in paths {
            // do not use `.unwrap()` here because we want a pretty error message like
            //
            // ```
            // thread 'config::tests::test_valid_config_fixtures' (196401) panicked at src/config.rs:250:59:
            // expected valid config tests/fixtures/config/valid/all_fields.toml: TOML parse error at line 4, column 16
            //   |
            // 4 | host = "0.0.0.0
            //   |                ^
            // invalid basic string, expected `"`
            // ```
            assert_valid_config(&path).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn test_invalid_config_fixtures() {
        let paths = fixture_paths("tests/fixtures/config/invalid")
            .expect("failed to load invalid fixtures");
        assert!(!paths.is_empty(), "no invalid fixtures found");
        for path in paths {
            // do not use `.unwrap()` here because we want a pretty error message like
            //
            // ```
            // thread 'config::tests::test_valid_config_fixtures' (196401) panicked at src/config.rs:250:59:
            // expected valid config tests/fixtures/config/valid/all_fields.toml: TOML parse error at line 4, column 16
            //   |
            // 4 | host = "0.0.0.0
            //   |                ^
            // invalid basic string, expected `"`
            // ```
            assert_invalid_config(&path).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn test_config_from_file_is_directory() {
        let result = Config::from_file(Path::new("/tmp"));
        assert!(result.is_err());
    }

    #[test]
    fn test_email_allowlist_mode_valid() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com", "@trusted.org"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let parsed = Config::parse(cfg);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_email_allowlist_rejects_deny_senders() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]
deny_senders = ["bob@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(
            err.to_string()
                .contains("email.deny_senders must be empty when email.mode = \"allowlist\"")
        );
    }

    #[test]
    fn test_email_denylist_requires_non_empty_list() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "denylist"

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(
            err.to_string()
                .contains("email.deny_senders must be non-empty when email.mode = \"denylist\"")
        );
    }

    #[test]
    fn test_email_rejects_invalid_sender_rule() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["not-an-email"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(err.to_string().contains("invalid allow_senders entry"));
    }

    #[test]
    fn test_email_rejects_invalid_default_max_results() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]
default_max_results = 0

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(
            err.to_string()
                .contains("email.default_max_results must be between 1 and 100")
        );
    }

    #[test]
    fn test_email_accepts_imap_credentials() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
imap_host = "imap.gmail.com"
imap_port = 993
"#;
        let parsed = Config::parse(cfg);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_email_rejects_missing_app_password() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(err.to_string().contains("missing field `app_password`"));
    }

    #[test]
    fn test_email_rejects_invalid_account_email() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
email = "botgmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(
            err.to_string()
                .contains("email.accounts[].email is invalid")
        );
    }

    #[test]
    fn test_email_rejects_access_token_field() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
access_token = "ya29.legacy"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(err.to_string().contains("unknown field `access_token`"));
    }

    #[test]
    fn test_email_rejects_oauth_fields() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
client_id = "google-client-id"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(err.to_string().contains("unknown field `client_id`"));
    }

    #[test]
    fn test_email_rejects_missing_email() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
app_password = "abcd efgh ijkl mnop"
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(err.to_string().contains("missing field `email`"));
    }

    #[test]
    fn test_email_rejects_invalid_imap_port() {
        let cfg = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
imap_port = 0
"#;
        let err = Config::parse(cfg).unwrap_err();
        assert!(
            err.to_string()
                .contains("email.accounts[].imap_port must be greater than 0")
        );
    }
}
