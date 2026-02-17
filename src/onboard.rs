use crate::platform;
use crate::tui;

use reqwest::StatusCode;
use reqwest::Url;
use reqwest::blocking::Client as BlockingHttpClient;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::warn;
use vfs::VfsPath;

const GOOGLE_DEFAULT_AUTH_URI: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_DEVICE_AUTH_URI: &str = "https://oauth2.googleapis.com/device/code";
const GOOGLE_DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const GOOGLE_DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_DEFAULT_REDIRECT_URI: &str = "urn:ietf:wg:oauth:2.0:oob";
const GMAIL_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
const GOOGLE_DEFAULT_DEVICE_CODE_INTERVAL_SECONDS: u64 = 5;
const GOOGLE_DEVICE_CODE_SLOWDOWN_SECONDS: u64 = 5;

/// API keys detected from an existing OpenClaw installation.
#[derive(Debug, Default)]
struct DetectedKeys {
    anthropic: Option<String>,
    openai: Option<String>,
}

impl DetectedKeys {
    /// Pick the key matching the given provider name.
    fn for_provider(&self, provider: &str) -> Option<&str> {
        match provider {
            "anthropic" => self.anthropic.as_deref(),
            "openai" => self.openai.as_deref(),
            _ => None,
        }
    }
}

/// Detect existing API keys from an OpenClaw installation.
///
/// Searches these locations in order:
/// 1. `auth-profiles.json` files inside `<state_dir>/agents/*/agent/`
/// 2. `.env` file in `<state_dir>`
/// 3. Environment variables `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`
fn detect_openclaw_api_keys() -> DetectedKeys {
    detect_openclaw_api_keys_with_home(std::env::var("HOME").ok().as_deref())
}

/// Inner implementation that accepts an explicit home dir for testability.
fn detect_openclaw_api_keys_with_home(home: Option<&str>) -> DetectedKeys {
    let root = crate::process::physical_root();
    match home {
        Some(h) => match root.join(h.trim_start_matches('/')) {
            Ok(home_vfs) => detect_openclaw_api_keys_vfs(&home_vfs),
            Err(_) => DetectedKeys {
                anthropic: std::env::var("ANTHROPIC_API_KEY").ok(),
                openai: std::env::var("OPENAI_API_KEY").ok(),
            },
        },
        None => DetectedKeys {
            anthropic: std::env::var("ANTHROPIC_API_KEY").ok(),
            openai: std::env::var("OPENAI_API_KEY").ok(),
        },
    }
}

/// VFS implementation of API key detection from filesystem sources.
/// Falls back to environment variables for any keys not found on the filesystem.
fn detect_openclaw_api_keys_vfs(home: &VfsPath) -> DetectedKeys {
    let mut keys = DetectedKeys::default();

    // Find the state directory
    let state_dir = match find_state_dir_vfs(home) {
        Some(d) => d,
        None => {
            // Fall back to env vars only
            keys.anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
            keys.openai = std::env::var("OPENAI_API_KEY").ok();
            return keys;
        }
    };

    // Strategy 1: auth-profiles.json
    try_auth_profiles_vfs(&state_dir, &mut keys);

    // Strategy 2: .env file
    if keys.anthropic.is_none() || keys.openai.is_none() {
        try_dot_env_vfs(&state_dir, &mut keys);
    }

    // Strategy 3: environment variables
    if keys.anthropic.is_none() {
        keys.anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
    }
    if keys.openai.is_none() {
        keys.openai = std::env::var("OPENAI_API_KEY").ok();
    }

    keys
}

/// Find the first existing OpenClaw state directory (VFS variant).
fn find_state_dir_vfs(home: &VfsPath) -> Option<VfsPath> {
    let candidates = [".openclaw", ".clawdbot", ".moltbot", ".moldbot"];
    for name in &candidates {
        if let Ok(path) = home.join(name) {
            if path.exists().unwrap_or(false) {
                return Some(path);
            }
        }
    }
    None
}

/// Scan auth-profiles.json files for API keys (VFS variant).
fn try_auth_profiles_vfs(state_dir: &VfsPath, keys: &mut DetectedKeys) {
    let agents_dir = match state_dir.join("agents") {
        Ok(d) => d,
        Err(_) => return,
    };
    let entries = match agents_dir.read_dir() {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        let profile_path = match entry.join("agent/auth-profiles.json") {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Ok(content) = profile_path.read_to_string()
            && let Ok(json) = serde_json::from_str::<Value>(&content)
            && let Some(profiles) = json.get("profiles").and_then(|p| p.as_object())
        {
            if keys.anthropic.is_none()
                && let Some(key) = profiles
                    .get("anthropic:default")
                    .and_then(|p| p.get("key"))
                    .and_then(|k| k.as_str())
                && !key.is_empty()
            {
                keys.anthropic = Some(key.to_string());
            }
            if keys.openai.is_none()
                && let Some(key) = profiles
                    .get("openai:default")
                    .and_then(|p| p.get("key"))
                    .and_then(|k| k.as_str())
                && !key.is_empty()
            {
                keys.openai = Some(key.to_string());
            }
        }
        if keys.anthropic.is_some() && keys.openai.is_some() {
            break;
        }
    }
}

/// Parse a .env file for API keys (VFS variant).
fn try_dot_env_vfs(state_dir: &VfsPath, keys: &mut DetectedKeys) {
    let env_path = match state_dir.join(".env") {
        Ok(p) => p,
        Err(_) => return,
    };
    let content = match env_path.read_to_string() {
        Ok(c) => c,
        Err(_) => return,
    };
    parse_dot_env_content(&content, keys);
}

/// Shared .env parsing logic.
fn parse_dot_env_content(content: &str, keys: &mut DetectedKeys) {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if v.is_empty() {
                continue;
            }
            if k == "ANTHROPIC_API_KEY" && keys.anthropic.is_none() {
                keys.anthropic = Some(v.to_string());
            } else if k == "OPENAI_API_KEY" && keys.openai.is_none() {
                keys.openai = Some(v.to_string());
            }
        }
    }
}

/// Collected onboarding configuration from user prompts.
#[derive(Debug, Clone)]
pub struct OnboardConfig {
    pub provider: String,
    pub model: String,
    pub real_api_key: String,
    pub virtual_api_key: String,
    pub openclaw_config_path: PathBuf,
    pub server_host: String,
    pub server_port: u16,
    pub gmail: Option<OnboardGmailConfig>,
}

/// Optional Gmail endpoint settings collected during onboarding.
#[derive(Debug, Clone)]
pub struct OnboardGmailConfig {
    pub mode: OnboardGmailMode,
    pub sender_rules: Vec<String>,
    pub account_virtual_key: String,
    pub user_id: String,
    pub refresh_token: String,
    pub client_secret_file: String,
    pub client_secret_json: String,
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

pub const OPENCLAW_GMAIL_MESSAGES_SKILL_NAME: &str = "get-gmail-messages";

#[derive(Debug, Deserialize)]
struct GoogleOAuthClientSecretsFile {
    #[serde(default)]
    installed: Option<GoogleOAuthClientEntry>,
    #[serde(default)]
    web: Option<GoogleOAuthClientEntry>,
}

#[derive(Debug, Deserialize)]
struct GoogleOAuthClientEntry {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    auth_uri: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
    #[serde(default)]
    redirect_uris: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct GoogleOAuthClientConfig {
    client_id: String,
    client_secret: String,
    auth_uri: String,
    token_uri: String,
    redirect_uri: String,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenExchangeResponse {
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleOAuthErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleDeviceCodeResponse {
    #[serde(default)]
    device_code: Option<String>,
    #[serde(default)]
    user_code: Option<String>,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_url: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Clone)]
struct GoogleDeviceCodeSession {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in_seconds: u64,
    interval_seconds: u64,
}

enum DeviceFlowError {
    Unavailable(String),
    Failed(String),
}

/// Sender filtering mode for the Gmail endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardGmailMode {
    Allowlist,
    Denylist,
}

impl OnboardGmailMode {
    fn as_toml_value(self) -> &'static str {
        match self {
            OnboardGmailMode::Allowlist => "allowlist",
            OnboardGmailMode::Denylist => "denylist",
        }
    }
}

/// Return the default OpenClaw config path.
pub fn default_openclaw_config_path() -> String {
    if let Ok(home) = std::env::var("HOME") {
        format!("{}/.openclaw/openclaw.json", home)
    } else {
        "~/.openclaw/openclaw.json".to_string()
    }
}

/// Try to load an existing onboarding configuration from the config directory.
/// Returns `None` if no previous config exists or it can't be read.
fn load_existing_config() -> Option<ExistingConfig> {
    let root = crate::process::physical_root();
    let config_dir = root.join("etc/clawshell").ok()?;
    load_existing_config_from_vfs(&config_dir)
}

/// VFS implementation for loading existing config from a directory.
fn load_existing_config_from_vfs(config_dir: &VfsPath) -> Option<ExistingConfig> {
    let config_file = config_dir.join("config.json").ok()?;
    let toml_file = config_dir.join("clawshell.toml").ok()?;

    let mut existing = ExistingConfig::default();

    // Read config.json for provider, model, virtual_api_key, openclaw_config_path
    if let Ok(content) = config_file.read_to_string()
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
    {
        existing.provider = json
            .get("provider")
            .and_then(|v| v.as_str())
            .map(String::from);
        existing.model = json.get("model").and_then(|v| v.as_str()).map(String::from);
        existing.real_api_key = json
            .get("real_api_key")
            .and_then(|v| v.as_str())
            .map(String::from);
        existing.virtual_api_key = json
            .get("virtual_api_key")
            .and_then(|v| v.as_str())
            .map(String::from);
        existing.openclaw_config_path = json
            .get("openclaw_config_path")
            .and_then(|v| v.as_str())
            .map(String::from);
    }

    // Read clawshell.toml for server host/port and optional Gmail settings
    if let Ok(content) = toml_file.read_to_string()
        && let Ok(toml) = content.parse::<toml::Table>()
    {
        if let Some(server) = toml.get("server").and_then(|s| s.as_table()) {
            existing.server_host = server
                .get("host")
                .and_then(|v| v.as_str())
                .map(String::from);
            existing.server_port = server
                .get("port")
                .and_then(|v| v.as_integer())
                .map(|p| p.to_string());
        }

        if let Some(gmail) = toml.get("gmail").and_then(|g| g.as_table()) {
            existing.gmail_enabled = gmail.get("enabled").and_then(|v| v.as_bool());
            existing.gmail_mode = gmail
                .get("mode")
                .and_then(|v| v.as_str())
                .and_then(parse_gmail_mode);

            let allow_rules = gmail
                .get("allow_senders")
                .and_then(|v| v.as_array())
                .map(|values| parse_string_array(values))
                .unwrap_or_default();
            let deny_rules = gmail
                .get("deny_senders")
                .and_then(|v| v.as_array())
                .map(|values| parse_string_array(values))
                .unwrap_or_default();
            existing.gmail_sender_rules = if !allow_rules.is_empty() {
                allow_rules
            } else {
                deny_rules
            };

            if let Some(account) = select_existing_gmail_account(
                gmail
                    .get("accounts")
                    .and_then(|v| v.as_array().map(Vec::as_slice)),
                existing.virtual_api_key.as_deref(),
            ) {
                existing.gmail_account_virtual_key = account
                    .get("virtual_key")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                existing.gmail_user_id = account
                    .get("user_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                existing.gmail_refresh_token = account
                    .get("refresh_token")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                existing.gmail_client_secret_file = account
                    .get("client_secret_file")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
        }
    }

    if existing.has_any() {
        Some(existing)
    } else {
        None
    }
}

fn parse_gmail_mode(value: &str) -> Option<OnboardGmailMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "allowlist" => Some(OnboardGmailMode::Allowlist),
        "denylist" => Some(OnboardGmailMode::Denylist),
        _ => None,
    }
}

fn parse_string_array(values: &[toml::Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.as_str())
        .map(String::from)
        .collect()
}

fn parse_google_oauth_client_config(content: &str) -> Result<GoogleOAuthClientConfig, String> {
    let parsed: GoogleOAuthClientSecretsFile =
        serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;
    let oauth_entry = parsed
        .installed
        .or(parsed.web)
        .ok_or("must contain an 'installed' or 'web' object".to_string())?;
    let client_id = oauth_entry
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("missing non-empty client_id under 'installed' or 'web'".to_string())?
        .to_string();
    let client_secret = oauth_entry
        .client_secret
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    let auth_uri = oauth_entry
        .auth_uri
        .as_deref()
        .unwrap_or(GOOGLE_DEFAULT_AUTH_URI)
        .trim()
        .to_string();
    let token_uri = oauth_entry
        .token_uri
        .as_deref()
        .unwrap_or(GOOGLE_DEFAULT_TOKEN_URI)
        .trim()
        .to_string();
    let redirect_uri = oauth_entry
        .redirect_uris
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or(GOOGLE_DEFAULT_REDIRECT_URI)
        .to_string();

    Url::parse(&auth_uri).map_err(|e| format!("invalid auth_uri '{auth_uri}': {e}"))?;
    Url::parse(&token_uri).map_err(|e| format!("invalid token_uri '{token_uri}': {e}"))?;

    Ok(GoogleOAuthClientConfig {
        client_id,
        client_secret,
        auth_uri,
        token_uri,
        redirect_uri,
    })
}

fn parse_google_oauth_error_response(body: &str) -> Option<GoogleOAuthErrorResponse> {
    let parsed: GoogleOAuthErrorResponse = serde_json::from_str(body).ok()?;
    if parsed.error.is_none() && parsed.error_description.is_none() {
        return None;
    }
    Some(parsed)
}

fn format_google_oauth_error_details(
    status: StatusCode,
    error: Option<&str>,
    error_description: Option<&str>,
    body: &str,
) -> String {
    if let Some(error) = error.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some(detail) = error_description
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return format!("{status}: {error} ({detail})");
        }
        return format!("{status}: {error}");
    }

    let body = body.trim();
    if body.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {body}")
    }
}

fn is_device_flow_unavailable_error(
    status: StatusCode,
    error: Option<&str>,
    error_description: Option<&str>,
) -> bool {
    if !status.is_client_error() {
        return false;
    }

    let error = error
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match error.as_str() {
        "unauthorized_client" | "unsupported_grant_type" | "invalid_client" => true,
        "invalid_request" => {
            let detail = error_description
                .map(str::trim)
                .unwrap_or_default()
                .to_ascii_lowercase();
            detail.contains("device")
                || detail.contains("unsupported")
                || detail.contains("not enabled")
        }
        _ => false,
    }
}

fn parse_google_device_code_response(body: &str) -> Result<GoogleDeviceCodeSession, String> {
    let parsed: GoogleDeviceCodeResponse = serde_json::from_str(body)
        .map_err(|e| format!("invalid device authorization response JSON: {e}"))?;

    let device_code = parsed
        .device_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("device authorization response missing non-empty device_code".to_string())?
        .to_string();
    let user_code = parsed
        .user_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("device authorization response missing non-empty user_code".to_string())?
        .to_string();
    let verification_uri = parsed
        .verification_uri
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            parsed
                .verification_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .ok_or(
            "device authorization response missing verification_uri/verification_url".to_string(),
        )?
        .to_string();
    let verification_uri_complete = parsed
        .verification_uri_complete
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    Url::parse(&verification_uri)
        .map_err(|e| format!("invalid verification_uri '{verification_uri}': {e}"))?;
    if let Some(url) = verification_uri_complete.as_deref() {
        Url::parse(url).map_err(|e| format!("invalid verification_uri_complete '{url}': {e}"))?;
    }

    Ok(GoogleDeviceCodeSession {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in_seconds: parsed.expires_in.unwrap_or(1800).max(1),
        interval_seconds: parsed
            .interval
            .unwrap_or(GOOGLE_DEFAULT_DEVICE_CODE_INTERVAL_SECONDS)
            .max(1),
    })
}

fn request_google_device_code(
    oauth: &GoogleOAuthClientConfig,
) -> Result<GoogleDeviceCodeSession, DeviceFlowError> {
    let client = BlockingHttpClient::new();
    let form_fields = [
        ("client_id", oauth.client_id.as_str()),
        ("scope", GMAIL_READONLY_SCOPE),
    ];
    let response = client
        .post(GOOGLE_DEVICE_AUTH_URI)
        .form(&form_fields)
        .send()
        .map_err(|e| {
            DeviceFlowError::Failed(format!("device authorization request failed: {e}"))
        })?;
    let status = response.status();
    let body = response.text().map_err(|e| {
        DeviceFlowError::Failed(format!(
            "failed to read device authorization response body: {e}"
        ))
    })?;
    if !status.is_success() {
        let parsed_error = parse_google_oauth_error_response(&body);
        let error_code = parsed_error
            .as_ref()
            .and_then(|value| value.error.as_deref());
        let error_description = parsed_error
            .as_ref()
            .and_then(|value| value.error_description.as_deref());
        let detail =
            format_google_oauth_error_details(status, error_code, error_description, &body);
        if is_device_flow_unavailable_error(status, error_code, error_description) {
            return Err(DeviceFlowError::Unavailable(format!(
                "device authorization endpoint returned {detail}"
            )));
        }
        return Err(DeviceFlowError::Failed(format!(
            "device authorization endpoint returned {detail}"
        )));
    }

    parse_google_device_code_response(&body).map_err(DeviceFlowError::Failed)
}

fn poll_google_device_token_for_refresh_token(
    oauth: &GoogleOAuthClientConfig,
    session: &GoogleDeviceCodeSession,
) -> Result<String, String> {
    let client = BlockingHttpClient::new();
    let deadline = Instant::now() + Duration::from_secs(session.expires_in_seconds);
    let mut interval_seconds = session.interval_seconds;

    loop {
        if Instant::now() >= deadline {
            return Err(
                "device authorization timed out before refresh_token was issued".to_string(),
            );
        }

        std::thread::sleep(Duration::from_secs(interval_seconds));

        let mut form_fields = vec![
            ("client_id", oauth.client_id.as_str()),
            ("device_code", session.device_code.as_str()),
            ("grant_type", GOOGLE_DEVICE_CODE_GRANT_TYPE),
        ];
        if !oauth.client_secret.trim().is_empty() {
            form_fields.push(("client_secret", oauth.client_secret.as_str()));
        }
        let response = client
            .post(&oauth.token_uri)
            .form(&form_fields)
            .send()
            .map_err(|e| format!("device token polling request failed: {e}"))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|e| format!("failed to read device token polling response body: {e}"))?;

        if status.is_success() {
            let parsed: GoogleTokenExchangeResponse = serde_json::from_str(&body)
                .map_err(|e| format!("invalid token response JSON: {e}"))?;
            if let Some(error) = parsed.error.as_deref() {
                match error {
                    "authorization_pending" => continue,
                    "slow_down" => {
                        interval_seconds =
                            interval_seconds.saturating_add(GOOGLE_DEVICE_CODE_SLOWDOWN_SECONDS);
                        continue;
                    }
                    _ => {
                        let detail = parsed
                            .error_description
                            .as_deref()
                            .unwrap_or_default()
                            .trim();
                        if detail.is_empty() {
                            return Err(format!("token endpoint error: {error}"));
                        }
                        return Err(format!("token endpoint error: {error} ({detail})"));
                    }
                }
            }

            return parsed
                .refresh_token
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .ok_or(
                    "token response missing refresh_token; revoke app access in your Google account and try again"
                        .to_string(),
                );
        }

        let parsed_error = parse_google_oauth_error_response(&body);
        let error_code = parsed_error
            .as_ref()
            .and_then(|value| value.error.as_deref());
        let error_description = parsed_error
            .as_ref()
            .and_then(|value| value.error_description.as_deref());
        match error_code
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "authorization_pending" => continue,
            "slow_down" => {
                interval_seconds =
                    interval_seconds.saturating_add(GOOGLE_DEVICE_CODE_SLOWDOWN_SECONDS);
                continue;
            }
            "access_denied" => {
                let detail = error_description.map(str::trim).unwrap_or_default();
                if detail.is_empty() {
                    return Err("device authorization was denied by user".to_string());
                }
                return Err(format!("device authorization was denied by user: {detail}"));
            }
            "expired_token" => {
                return Err("device authorization code expired before completion".to_string());
            }
            _ => {
                let detail =
                    format_google_oauth_error_details(status, error_code, error_description, &body);
                return Err(format!("token polling endpoint returned {detail}"));
            }
        }
    }
}

fn request_gmail_refresh_token_via_device_flow(
    oauth: &GoogleOAuthClientConfig,
) -> Result<String, DeviceFlowError> {
    loop {
        let session = match request_google_device_code(oauth) {
            Ok(session) => session,
            Err(DeviceFlowError::Unavailable(error)) => {
                return Err(DeviceFlowError::Unavailable(error));
            }
            Err(DeviceFlowError::Failed(error)) => {
                tui::print_warning(&format!(
                    "Failed to start device authorization flow: {error}"
                ));
                let retry = tui::prompt_confirm("Retry device authorization flow?", true)
                    .map_err(|e| DeviceFlowError::Failed(e.to_string()))?;
                if retry {
                    continue;
                }
                return Err(DeviceFlowError::Failed(
                    "Device OAuth flow cancelled before obtaining refresh_token".to_string(),
                ));
            }
        };

        if let Some(url) = session.verification_uri_complete.as_deref() {
            tui::print_info("Google verification URL", url);
        }
        tui::print_info("Google verification page", &session.verification_uri);
        tui::print_info("Google device user_code", &session.user_code);
        tui::print_warning(
            "Open the verification URL in any browser-enabled machine, enter the user code, and approve access.",
        );
        let wait_hint = format!(
            "Polling token endpoint for up to {} seconds...",
            session.expires_in_seconds
        );
        tui::print_info("Device OAuth", &wait_hint);

        match poll_google_device_token_for_refresh_token(oauth, &session) {
            Ok(token) => return Ok(token),
            Err(error) => {
                tui::print_warning(&format!("Device authorization failed: {error}"));
                let retry = tui::prompt_confirm("Retry device authorization flow?", true)
                    .map_err(|e| DeviceFlowError::Failed(e.to_string()))?;
                if !retry {
                    return Err(DeviceFlowError::Failed(
                        "Device OAuth flow cancelled before obtaining refresh_token".to_string(),
                    ));
                }
            }
        }
    }
}

fn request_gmail_refresh_token(oauth: &GoogleOAuthClientConfig) -> Result<String, String> {
    match request_gmail_refresh_token_via_device_flow(oauth) {
        Ok(token) => Ok(token),
        Err(DeviceFlowError::Unavailable(error)) => {
            tui::print_warning(&format!("Device authorization flow unavailable: {error}"));
            tui::print_warning(
                "Falling back to redirect URL/code flow. Open links on another machine if needed.",
            );
            request_gmail_refresh_token_via_oauth(oauth)
        }
        Err(DeviceFlowError::Failed(error)) => Err(error),
    }
}

fn build_google_consent_url(oauth: &GoogleOAuthClientConfig) -> Result<String, String> {
    let mut url = Url::parse(&oauth.auth_uri)
        .map_err(|e| format!("invalid OAuth auth_uri '{}': {e}", oauth.auth_uri))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", &oauth.client_id);
        query.append_pair("redirect_uri", &oauth.redirect_uri);
        query.append_pair("response_type", "code");
        query.append_pair("scope", GMAIL_READONLY_SCOPE);
        query.append_pair("access_type", "offline");
        query.append_pair("prompt", "consent");
    }
    Ok(url.to_string())
}

fn extract_google_authorization_code(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("authorization code input cannot be empty".to_string());
    }

    if let Ok(url) = Url::parse(input) {
        let mut code = None;
        let mut error = None;
        let mut error_description = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                "error_description" => error_description = Some(value.into_owned()),
                _ => {}
            }
        }
        if let Some(err) = error {
            let detail = error_description.unwrap_or_default();
            if detail.is_empty() {
                return Err(format!("authorization failed: {err}"));
            }
            return Err(format!("authorization failed: {err} ({detail})"));
        }
        return code
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or("redirect URL is missing non-empty query parameter 'code'".to_string());
    }

    if input.starts_with("code=") {
        let synthetic = format!("https://localhost/?{input}");
        if let Ok(url) = Url::parse(&synthetic) {
            return extract_google_authorization_code(url.as_str());
        }
    }

    Ok(input.to_string())
}

fn exchange_google_auth_code_for_refresh_token(
    oauth: &GoogleOAuthClientConfig,
    code: &str,
) -> Result<String, String> {
    let client = BlockingHttpClient::new();
    let mut form_fields = vec![
        ("client_id", oauth.client_id.as_str()),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", oauth.redirect_uri.as_str()),
    ];
    if !oauth.client_secret.trim().is_empty() {
        form_fields.push(("client_secret", oauth.client_secret.as_str()));
    }

    let response = client
        .post(&oauth.token_uri)
        .form(&form_fields)
        .send()
        .map_err(|e| format!("token exchange request failed: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| format!("failed to read token response body: {e}"))?;
    if !status.is_success() {
        return Err(format!("token endpoint returned {status}: {body}"));
    }

    let parsed: GoogleTokenExchangeResponse =
        serde_json::from_str(&body).map_err(|e| format!("invalid token response JSON: {e}"))?;
    if let Some(error) = parsed.error {
        let detail = parsed.error_description.unwrap_or_default();
        if detail.is_empty() {
            return Err(format!("token endpoint error: {error}"));
        }
        return Err(format!("token endpoint error: {error} ({detail})"));
    }

    parsed
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or(
            "token response missing refresh_token; revoke app access in your Google account and try again"
                .to_string(),
        )
}

fn request_gmail_refresh_token_via_oauth(
    oauth: &GoogleOAuthClientConfig,
) -> Result<String, String> {
    let consent_url = build_google_consent_url(oauth)?;
    tui::print_info("Google consent URL", &consent_url);
    tui::print_warning(
        "Open the URL in a browser, approve access, then paste the full redirect URL (preferred) or code.",
    );

    loop {
        let input = tui::prompt_text("Paste redirect URL or authorization code", None)
            .map_err(|e| e.to_string())?;
        let code = match extract_google_authorization_code(&input) {
            Ok(code) => code,
            Err(error) => {
                tui::print_warning(&format!("Invalid authorization input: {error}"));
                continue;
            }
        };

        match exchange_google_auth_code_for_refresh_token(oauth, &code) {
            Ok(token) => return Ok(token),
            Err(error) => {
                tui::print_warning(&format!("Failed to exchange authorization code: {error}"));
                let retry = tui::prompt_confirm("Try OAuth exchange again?", true)
                    .map_err(|e| e.to_string())?;
                if !retry {
                    return Err("OAuth flow cancelled before obtaining refresh_token".to_string());
                }
            }
        }
    }
}

fn select_existing_gmail_account<'a>(
    accounts: Option<&'a [toml::Value]>,
    preferred_virtual_key: Option<&str>,
) -> Option<&'a toml::value::Table> {
    let accounts = accounts?;

    if let Some(preferred) = preferred_virtual_key
        && let Some(account) = accounts.iter().filter_map(|v| v.as_table()).find(|table| {
            table
                .get("virtual_key")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == preferred)
        })
    {
        return Some(account);
    }

    accounts.iter().find_map(|v| v.as_table())
}

/// Previously saved configuration values used as defaults during re-onboarding.
#[derive(Default)]
struct ExistingConfig {
    provider: Option<String>,
    model: Option<String>,
    real_api_key: Option<String>,
    virtual_api_key: Option<String>,
    openclaw_config_path: Option<String>,
    server_host: Option<String>,
    server_port: Option<String>,
    gmail_enabled: Option<bool>,
    gmail_mode: Option<OnboardGmailMode>,
    gmail_sender_rules: Vec<String>,
    gmail_account_virtual_key: Option<String>,
    gmail_user_id: Option<String>,
    gmail_refresh_token: Option<String>,
    gmail_client_secret_file: Option<String>,
}

impl ExistingConfig {
    fn has_any(&self) -> bool {
        self.provider.is_some()
            || self.model.is_some()
            || self.real_api_key.is_some()
            || self.virtual_api_key.is_some()
            || self.openclaw_config_path.is_some()
            || self.server_host.is_some()
            || self.server_port.is_some()
            || self.gmail_enabled.is_some()
            || self.gmail_mode.is_some()
            || !self.gmail_sender_rules.is_empty()
            || self.gmail_account_virtual_key.is_some()
            || self.gmail_user_id.is_some()
            || self.gmail_refresh_token.is_some()
            || self.gmail_client_secret_file.is_some()
    }
}

fn parse_sender_rules(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn sender_rule_has_valid_shape(rule: &str) -> bool {
    if let Some(domain) = rule.strip_prefix('@') {
        !domain.is_empty() && domain.contains('.') && !domain.contains(' ')
    } else {
        let mut parts = rule.split('@');
        let local = parts.next().unwrap_or_default();
        let domain = parts.next().unwrap_or_default();
        parts.next().is_none()
            && !local.is_empty()
            && !domain.is_empty()
            && domain.contains('.')
            && !rule.contains(' ')
    }
}

fn mask_secret(secret: &str) -> String {
    if secret.len() > 8 {
        format!("{}...{}", &secret[..4], &secret[secret.len() - 4..])
    } else if secret.is_empty() {
        "(empty)".to_string()
    } else {
        "*".repeat(secret.len())
    }
}

/// Collect all onboarding information using the TUI (interactive terminal prompts).
/// If a previous configuration exists, its values are used as defaults.
pub fn collect_onboard_config_tui() -> Result<OnboardConfig, Box<dyn std::error::Error>> {
    let existing = load_existing_config();

    if existing.is_some() {
        tui::print_success("Existing configuration detected — using as defaults.");
        println!();
    }

    let existing = existing.unwrap_or_default();

    tui::print_section("API Configuration");

    // Provider selection — if existing, reorder so the existing choice is first
    let provider_options = if existing.provider.as_deref() == Some("anthropic") {
        vec!["Anthropic", "OpenAI"]
    } else {
        vec!["OpenAI", "Anthropic"]
    };
    let provider_choice = tui::prompt_select("Select a model provider", provider_options)?;
    let provider = match provider_choice {
        "Anthropic" => "anthropic".to_string(),
        _ => "openai".to_string(),
    };

    // Model name — use existing model or provider-specific default
    let default_model = existing
        .model
        .as_deref()
        .unwrap_or(if provider == "anthropic" {
            "claude-sonnet-4-5-20250929"
        } else {
            "gpt-5.2-chat-latest"
        });
    let model = tui::prompt_text("Enter the model name", Some(default_model))?;

    // Real API key — if ClawShell already has one, use it; otherwise try detecting from OpenClaw
    let is_first_onboard = existing.real_api_key.is_none();
    let effective_existing_key = if !is_first_onboard {
        existing.real_api_key.clone()
    } else {
        let detected = detect_openclaw_api_keys();
        let key = detected.for_provider(&provider).map(|s| s.to_string());
        if key.is_some() {
            tui::print_warning(
                "An API key was detected from your OpenClaw config. \
                 It is strongly recommended to generate a new key from your provider, \
                 enter it here instead, and revoke the old one.",
            );
        }
        key
    };

    let real_api_key = if let Some(ref existing_key) = effective_existing_key {
        // Show a truncated version so the user knows what key is on file
        let masked = mask_secret(existing_key);
        tui::print_info("Existing key", &masked);

        let prompt_msg = if is_first_onboard {
            // Key was detected from OpenClaw — strongly recommend rotating
            "Enter a NEW API key (recommended) or leave blank to reuse the detected key"
        } else {
            // Re-onboard — key already managed by ClawShell
            tui::print_warning(
                "Consider rotating your API key periodically. \
                 Generate a fresh key from your provider and enter it below.",
            );
            "Enter a new API key, or leave blank to keep the current one"
        };
        let input = tui::prompt_password(prompt_msg)?;
        if input.trim().is_empty() {
            existing_key.clone()
        } else {
            input
        }
    } else {
        let input = tui::prompt_password("Enter the real API key for the selected provider")?;
        if input.trim().is_empty() {
            return Err("API key cannot be empty".into());
        }
        input
    };

    // Virtual API key
    let fallback_virtual_key = format!("{{clawshell-virtual-key-{}}}", provider);
    let default_virtual = existing
        .virtual_api_key
        .as_deref()
        .unwrap_or(&fallback_virtual_key);
    let virtual_api_key = tui::prompt_text(
        "Enter the virtual API key for OpenClaw",
        Some(default_virtual),
    )?;

    tui::print_section("Gmail Configuration");

    let setup_gmail = tui::prompt_confirm(
        "Configure Gmail sender filtering endpoint (GET /v1/gmail/messages)?",
        existing.gmail_enabled.unwrap_or(false),
    )?;

    let gmail = if setup_gmail {
        let mode_options = if existing.gmail_mode == Some(OnboardGmailMode::Denylist) {
            vec!["Denylist", "Allowlist"]
        } else {
            vec!["Allowlist", "Denylist"]
        };
        let mode_choice = tui::prompt_select("Select Gmail sender filter mode", mode_options)?;
        let mode = match mode_choice {
            "Denylist" => OnboardGmailMode::Denylist,
            _ => OnboardGmailMode::Allowlist,
        };

        let default_sender_rules_owned =
            if existing.gmail_mode == Some(mode) && !existing.gmail_sender_rules.is_empty() {
                Some(existing.gmail_sender_rules.join(", "))
            } else {
                None
            };
        let sender_rules_prompt = match mode {
            OnboardGmailMode::Allowlist => {
                "Enter allow_senders (comma-separated emails or @domain rules)"
            }
            OnboardGmailMode::Denylist => {
                "Enter deny_senders (comma-separated emails or @domain rules)"
            }
        };
        let sender_rules_input = tui::prompt_text_validated(
            sender_rules_prompt,
            default_sender_rules_owned.as_deref(),
            |input: &str| {
                let rules = parse_sender_rules(input);
                if rules.is_empty() {
                    Ok(inquire::validator::Validation::Invalid(
                        "Enter at least one sender rule".into(),
                    ))
                } else if rules.iter().any(|rule| !sender_rule_has_valid_shape(rule)) {
                    Ok(inquire::validator::Validation::Invalid(
                        "Sender rules must be full emails or @domain entries".into(),
                    ))
                } else {
                    Ok(inquire::validator::Validation::Valid)
                }
            },
        )?;
        let sender_rules = parse_sender_rules(&sender_rules_input);

        let default_gmail_virtual_key_owned = existing
            .gmail_account_virtual_key
            .clone()
            .unwrap_or_else(|| virtual_api_key.clone());
        let account_virtual_key = tui::prompt_text(
            "Enter the virtual API key for Gmail endpoint access",
            Some(&default_gmail_virtual_key_owned),
        )?;
        if account_virtual_key.trim().is_empty() {
            return Err("Gmail virtual API key cannot be empty".into());
        }

        let default_user_id_owned = existing
            .gmail_user_id
            .clone()
            .unwrap_or_else(|| "me".to_string());
        let user_id = tui::prompt_text("Enter Gmail user_id", Some(&default_user_id_owned))?;
        if user_id.trim().is_empty() {
            return Err("Gmail user_id cannot be empty".into());
        }

        let client_secret_file = existing
            .gmail_client_secret_file
            .clone()
            .unwrap_or_else(|| "oauth/client_secret.json".to_string());
        tui::print_info("Gmail OAuth file", &client_secret_file);
        let (client_secret_json, oauth_client) = loop {
            let pasted =
                tui::prompt_multiline("Paste Gmail OAuth client_secret.json content", "EOF")?;
            if pasted.trim().is_empty() {
                tui::print_warning("Gmail client_secret.json content cannot be empty.");
                continue;
            }
            match parse_google_oauth_client_config(&pasted) {
                Ok(oauth_client) => break (pasted, oauth_client),
                Err(message) => {
                    tui::print_warning(&format!("Invalid client_secret.json: {message}"));
                }
            }
        };
        let refresh_token = if let Some(existing_refresh_token) =
            existing.gmail_refresh_token.as_deref()
        {
            tui::print_info(
                "Existing Gmail refresh_token",
                &mask_secret(existing_refresh_token),
            );
            let keep_existing = tui::prompt_confirm("Reuse existing Gmail refresh_token?", true)?;
            if keep_existing {
                existing_refresh_token.to_string()
            } else {
                request_gmail_refresh_token(&oauth_client)?
            }
        } else {
            request_gmail_refresh_token(&oauth_client)?
        };

        Some(OnboardGmailConfig {
            mode,
            sender_rules,
            account_virtual_key,
            user_id,
            refresh_token,
            client_secret_file,
            client_secret_json,
        })
    } else {
        None
    };

    tui::print_section("OpenClaw Configuration");

    // OpenClaw config path
    let fallback_openclaw_path = default_openclaw_config_path();
    let default_openclaw = existing
        .openclaw_config_path
        .as_deref()
        .unwrap_or(&fallback_openclaw_path);
    let openclaw_config_path = tui::prompt_text(
        "Enter the OpenClaw configuration file path",
        Some(default_openclaw),
    )?;

    // Server settings
    let default_host = existing.server_host.as_deref().unwrap_or("127.0.0.1");
    let default_port = existing.server_port.as_deref().unwrap_or("18790");
    let server_host = tui::prompt_text("Enter the ClawShell server IP", Some(default_host))?;
    let server_port_str = tui::prompt_text_validated(
        "Enter the ClawShell server port",
        Some(default_port),
        |input: &str| {
            if input.parse::<u16>().is_ok() {
                Ok(inquire::validator::Validation::Valid)
            } else {
                Ok(inquire::validator::Validation::Invalid(
                    "Please enter a valid port number (1-65535)".into(),
                ))
            }
        },
    )?;
    let server_port: u16 = server_port_str.parse().unwrap();

    Ok(OnboardConfig {
        provider,
        model,
        real_api_key,
        virtual_api_key,
        openclaw_config_path: PathBuf::from(openclaw_config_path),
        server_host,
        server_port,
        gmail,
    })
}

/// Generate the ClawShell TOML configuration content with the given key mapping.
pub fn generate_clawshell_config(config: &OnboardConfig) -> String {
    let mut output = format!(
        r#"# ClawShell Configuration
version = "{version}"
log_level = "info"

[server]
host = "{host}"
port = {port}

[upstream]
openai_base_url = "https://api.openai.com"
anthropic_base_url = "https://api.anthropic.com"

[[keys]]
virtual_key = {virtual_key}
real_key = {real_key}
provider = {provider}
[dlp]
scan_responses = true
patterns = [
    {{ name = "ssn",             regex = '\b\d{{3}}-\d{{2}}-\d{{4}}\b',                                             action = "redact" }},
    {{ name = "visa_card",       regex = '\b4[0-9]{{12}}(?:[0-9]{{3}})?\b',                                        action = "redact" }},
    {{ name = "visa_mastercard", regex = '\b(?:4[0-9]{{12}}(?:[0-9]{{3}})?|5[1-5][0-9]{{14}})\b',                  action = "redact" }},
    {{ name = "mastercard",      regex = '\b5[1-5][0-9]{{14}}\b',                                                  action = "redact" }},
    {{ name = "amex_card",       regex = '\b3[47][0-9]{{13}}\b',                                                   action = "redact" }},
]
"#,
        version = env!("CARGO_PKG_VERSION"),
        host = config.server_host,
        port = config.server_port,
        virtual_key = toml_string(&config.virtual_api_key),
        real_key = toml_string(&config.real_api_key),
        provider = toml_string(&config.provider),
    );

    if let Some(gmail) = &config.gmail {
        let (allow_senders, deny_senders) = match gmail.mode {
            OnboardGmailMode::Allowlist => (toml_string_array(&gmail.sender_rules), "[]".into()),
            OnboardGmailMode::Denylist => ("[]".into(), toml_string_array(&gmail.sender_rules)),
        };

        output.push_str(&format!(
            r#"
[gmail]
enabled = true
mode = "{mode}"
allow_senders = {allow_senders}
deny_senders = {deny_senders}
default_max_results = 50
api_base_url = "https://gmail.googleapis.com/"

[[gmail.accounts]]
virtual_key = {virtual_key}
user_id = {user_id}
"#,
            mode = gmail.mode.as_toml_value(),
            allow_senders = allow_senders,
            deny_senders = deny_senders,
            virtual_key = toml_string(&gmail.account_virtual_key),
            user_id = toml_string(&gmail.user_id),
        ));
        output.push_str(&format!(
            "refresh_token = {}\n",
            toml_string(&gmail.refresh_token)
        ));
        output.push_str(&format!(
            "client_secret_file = {}\n",
            toml_string(&gmail.client_secret_file)
        ));
    }

    output
}

fn format_clawshell_base_url(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

pub fn openclaw_config_root(openclaw_config_path: &Path) -> PathBuf {
    openclaw_config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn render_openclaw_gmail_messages_skill(config: &OnboardConfig) -> Option<OnboardSkillBundle> {
    let gmail = config.gmail.as_ref()?;
    let base_url = format_clawshell_base_url(&config.server_host, config.server_port);
    let virtual_key = gmail.account_virtual_key.as_str();

    let skill_md = format!(
        r#"---
name: get-gmail-messages
description: Get Gmail messages.
---

# Get Gmail Messages

Fetch Gmail message metadata through specific endpoint using `curl`.

## Request

- Method: `GET`
- Path: `/v1/gmail/messages`
- Base URL: `{base_url}`
- Authorization: `Bearer {virtual_key}`

```bash
curl -sS \
  -H "Authorization: Bearer {virtual_key}" \
  "{base_url}/v1/gmail/messages"
```

## Optional Query Parameters

- `q`
- `max_results` (1-100)
- `page_token`
- `include_spam_trash`

## Response

Top-level fields:
- `messages`
- `next_page_token`

Load `references/api-usage.md` for detailed examples and status-code behavior.
"#
    );

    let reference_md = format!(
        r#"# GET /v1/gmail/messages API Usage

## Endpoint

- URL: `{base_url}/v1/gmail/messages`
- Header: `Authorization: Bearer {virtual_key}`

## Examples

### Basic request

```bash
curl -sS \
  -H "Authorization: Bearer {virtual_key}" \
  "{base_url}/v1/gmail/messages"
```

### Query + pagination

```bash
curl -sS \
  -H "Authorization: Bearer {virtual_key}" \
  --get "{base_url}/v1/gmail/messages" \
  --data-urlencode "q=from:alice@example.com newer_than:7d" \
  --data-urlencode "max_results=25" \
  --data-urlencode "page_token=<next_page_token>"
```

## Notes

- `max_results` must be between 1 and 100.
- `next_page_token` is returned when more pages are available.
- Error payloads are JSON objects: `{{"error":"message"}}`.
"#
    );

    Some(OnboardSkillBundle {
        name: OPENCLAW_GMAIL_MESSAGES_SKILL_NAME,
        files: vec![
            OnboardSkillFile {
                relative_path: "SKILL.md",
                content: skill_md,
            },
            OnboardSkillFile {
                relative_path: "references/api-usage.md",
                content: reference_md,
            },
        ],
    })
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn toml_string_array(values: &[String]) -> String {
    toml::Value::Array(
        values
            .iter()
            .map(|value| toml::Value::String(value.to_string()))
            .collect(),
    )
    .to_string()
}

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

/// Modify the OpenClaw configuration JSON to add ClawShell entries.
///
/// This function:
/// 1. Sets `"CLAWSHELL_API_KEY"` in the `env` object
/// 2. Appends a model entry to `agents.defaults.models`
/// 3. Appends a provider entry to `models.providers`
pub fn modify_openclaw_config(
    content: &str,
    config: &OnboardConfig,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut json: Value = serde_json::from_str(content)?;

    // 1. Set CLAWSHELL_API_KEY in the env object
    ensure_nested_object(&mut json, &["env"]);
    json["env"]["CLAWSHELL_API_KEY"] = Value::String(config.virtual_api_key.clone());

    // 2. Add to agents.defaults.models (object map, not array)
    let model_key = format!("clawshell/{}", config.model);
    let model_value = serde_json::json!({
        "alias": "clawshell"
    });

    ensure_nested_object(&mut json, &["agents", "defaults", "models"]);
    json["agents"]["defaults"]["models"][&model_key] = model_value;

    // 3. Add to models.providers (object map, not array)
    let base_url = format!("http://{}:{}/v1", config.server_host, config.server_port);
    let provider_value = serde_json::json!({
        "baseUrl": base_url,
        "api": "openai-completions",
        "apiKey": "${CLAWSHELL_API_KEY}",
        "models": [
            {
                "id": config.model,
                "name": config.model
            }
        ]
    });

    ensure_nested_object(&mut json, &["models", "providers"]);
    json["models"]["providers"]["clawshell"] = provider_value;

    Ok(serde_json::to_string_pretty(&json)?)
}

/// Check if the OpenClaw config has a default model referencing clawshell.
///
/// Returns true if `agents.defaults.model` starts with `"clawshell/"` or equals `"clawshell"`.
pub fn is_clawshell_default_model(content: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let json: Value = serde_json::from_str(content)?;

    if let Some(model) = json
        .get("agents")
        .and_then(|a| a.get("defaults"))
        .and_then(|d| d.get("model"))
        .and_then(|m| m.as_str())
    {
        Ok(model.starts_with("clawshell/") || model == "clawshell")
    } else {
        Ok(false)
    }
}

/// Remove ClawShell entries from an OpenClaw configuration JSON string.
///
/// This function removes:
/// 1. The `"CLAWSHELL_API_KEY"` key from the `env` object
/// 2. All keys starting with `"clawshell/"` from `agents.defaults.models` object
/// 3. The `"clawshell"` key from `models.providers` object
pub fn remove_openclaw_entries(content: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut json: Value = serde_json::from_str(content)?;

    // 1. Remove CLAWSHELL_API_KEY from env object
    if let Some(env) = json.get_mut("env").and_then(|e| e.as_object_mut()) {
        env.remove("CLAWSHELL_API_KEY");
    }

    // 2. Remove clawshell/ keys from agents.defaults.models
    if let Some(models) = json
        .get_mut("agents")
        .and_then(|a| a.get_mut("defaults"))
        .and_then(|d| d.get_mut("models"))
        .and_then(|m| m.as_object_mut())
    {
        let keys_to_remove: Vec<String> = models
            .keys()
            .filter(|k| k.starts_with("clawshell/"))
            .cloned()
            .collect();
        for key in keys_to_remove {
            models.remove(&key);
        }
    }

    // 3. Remove the "clawshell" key from models.providers
    if let Some(providers) = json
        .get_mut("models")
        .and_then(|m| m.get_mut("providers"))
        .and_then(|p| p.as_object_mut())
    {
        providers.remove("clawshell");
    }

    Ok(serde_json::to_string_pretty(&json)?)
}

/// Ensure nested object keys exist in a JSON value.
fn ensure_nested_object(json: &mut Value, keys: &[&str]) {
    let mut current = json;
    for key in keys {
        if !current.get(*key).is_some_and(|v| v.is_object()) {
            current[*key] = serde_json::json!({});
        }
        current = current.get_mut(*key).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Auto-start service management (systemd / launchd)
// ---------------------------------------------------------------------------

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
    exe_path: &Path,
    config_path: &Path,
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
    use vfs::MemoryFS;

    fn test_config() -> OnboardConfig {
        OnboardConfig {
            provider: "openai".to_string(),
            model: "gpt-5.2".to_string(),
            real_api_key: "sk-real-key-123".to_string(),
            virtual_api_key: "{clawshell-virtual-key-openai}".to_string(),
            openclaw_config_path: PathBuf::from("/tmp/test-openclaw.json"),
            server_host: "127.0.0.1".to_string(),
            server_port: 18790,
            gmail: None,
        }
    }

    /// Create a VFS helper that writes content to a path, creating parent dirs.
    fn vfs_write(root: &VfsPath, path: &str, content: &str) {
        let p = root.join(path).unwrap();
        p.parent().create_dir_all().unwrap();
        p.create_file()
            .unwrap()
            .write_all(content.as_bytes())
            .unwrap();
    }

    #[test]
    fn test_generate_clawshell_config() {
        let config = test_config();
        let toml_str = generate_clawshell_config(&config);
        assert!(toml_str.contains("host = \"127.0.0.1\""));
        assert!(toml_str.contains("port = 18790"));
        assert!(toml_str.contains("virtual_key = \"{clawshell-virtual-key-openai}\""));
        assert!(toml_str.contains("real_key = \"sk-real-key-123\""));
        assert!(toml_str.contains("provider = \"openai\""));
        assert!(toml_str.contains("log_level = \"info\""));
        assert!(toml_str.contains(&format!("version = \"{}\"", env!("CARGO_PKG_VERSION"))));
        assert!(toml_str.contains("[dlp]"));
        assert!(!toml_str.contains("[gmail]"));
        assert!(!toml_str.contains("[rate_limit]"));
    }

    #[test]
    fn test_generate_config_anthropic() {
        let mut config = test_config();
        config.provider = "anthropic".to_string();
        config.model = "claude-sonnet-4-5-20250929".to_string();
        let toml_str = generate_clawshell_config(&config);
        assert!(toml_str.contains("provider = \"anthropic\""));
    }

    #[test]
    fn test_sender_rule_shape_validation() {
        assert!(sender_rule_has_valid_shape("alice@example.com"));
        assert!(sender_rule_has_valid_shape("@trusted.org"));
        assert!(!sender_rule_has_valid_shape("aliceexample.com"));
        assert!(!sender_rule_has_valid_shape("@"));
        assert!(!sender_rule_has_valid_shape("alice@localhost"));
    }

    #[test]
    fn test_generate_clawshell_config_with_gmail_refresh_token() {
        let mut config = test_config();
        config.gmail = Some(OnboardGmailConfig {
            mode: OnboardGmailMode::Denylist,
            sender_rules: vec!["@blocked.com".to_string()],
            account_virtual_key: "{gmail-virtual-key}".to_string(),
            user_id: "me".to_string(),
            refresh_token: "1//refresh".to_string(),
            client_secret_file: "/etc/clawshell/client_secret.json".to_string(),
            client_secret_json: "{}".to_string(),
        });

        let toml_str = generate_clawshell_config(&config);
        assert!(toml_str.contains("[gmail]"));
        assert!(toml_str.contains("enabled = true"));
        assert!(toml_str.contains("mode = \"denylist\""));
        assert!(toml_str.contains("allow_senders = []"));
        assert!(toml_str.contains("deny_senders = [\"@blocked.com\"]"));
        assert!(toml_str.contains("refresh_token = \"1//refresh\""));
        assert!(toml_str.contains("client_secret_file = \"/etc/clawshell/client_secret.json\""));
        assert!(!toml_str.contains("client_id ="));
        assert!(!toml_str.contains("client_secret ="));
        assert!(!toml_str.contains("token_uri ="));
    }

    #[test]
    fn test_openclaw_config_root_from_file_path() {
        let path = PathBuf::from("/home/user/.openclaw/openclaw.json");
        assert_eq!(
            openclaw_config_root(&path),
            PathBuf::from("/home/user/.openclaw")
        );
    }

    #[test]
    fn test_render_openclaw_gmail_messages_skill_returns_none_without_gmail() {
        let config = test_config();
        assert!(render_openclaw_gmail_messages_skill(&config).is_none());
    }

    #[test]
    fn test_render_openclaw_gmail_messages_skill_renders_concrete_values() {
        let mut config = test_config();
        config.gmail = Some(OnboardGmailConfig {
            mode: OnboardGmailMode::Allowlist,
            sender_rules: vec!["@trusted.org".to_string()],
            account_virtual_key: "vk-gmail-001".to_string(),
            user_id: "me".to_string(),
            refresh_token: "1//refresh".to_string(),
            client_secret_file: "oauth/client_secret.json".to_string(),
            client_secret_json: "{}".to_string(),
        });

        let skill = render_openclaw_gmail_messages_skill(&config).unwrap();
        assert_eq!(skill.name, OPENCLAW_GMAIL_MESSAGES_SKILL_NAME);
        assert_eq!(skill.files.len(), 2);

        let skill_md = skill
            .files
            .iter()
            .find(|file| file.relative_path == "SKILL.md")
            .unwrap()
            .content
            .as_str();
        assert!(skill_md.contains("http://127.0.0.1:18790"));
        assert!(skill_md.contains("Bearer vk-gmail-001"));
        assert!(!skill_md.contains("CLAWSHELL_BASE_URL"));
        assert!(!skill_md.contains("VIRTUAL_KEY"));
    }

    #[test]
    fn test_parse_google_oauth_client_config_accepts_installed() {
        let json = r#"{"installed":{"client_id":"id.apps.googleusercontent.com","client_secret":"secret-123","redirect_uris":["http://localhost/callback"]}}"#;
        let parsed = parse_google_oauth_client_config(json).unwrap();
        assert_eq!(parsed.client_id, "id.apps.googleusercontent.com");
        assert_eq!(parsed.client_secret, "secret-123");
        assert_eq!(parsed.redirect_uri, "http://localhost/callback");
    }

    #[test]
    fn test_parse_google_oauth_client_config_accepts_web_with_defaults() {
        let json = r#"{"web":{"client_id":"web-id.apps.googleusercontent.com"}}"#;
        let parsed = parse_google_oauth_client_config(json).unwrap();
        assert_eq!(parsed.client_id, "web-id.apps.googleusercontent.com");
        assert_eq!(parsed.auth_uri, GOOGLE_DEFAULT_AUTH_URI);
        assert_eq!(parsed.token_uri, GOOGLE_DEFAULT_TOKEN_URI);
        assert_eq!(parsed.redirect_uri, GOOGLE_DEFAULT_REDIRECT_URI);
    }

    #[test]
    fn test_parse_google_oauth_client_config_rejects_missing_client_id() {
        let json = r#"{"installed":{"client_secret":"secret"}}"#;
        assert!(parse_google_oauth_client_config(json).is_err());
    }

    #[test]
    fn test_extract_google_authorization_code_from_redirect_url() {
        let input = "https://localhost/?state=abc&code=4/0AQSTgQ-example&scope=email";
        let code = extract_google_authorization_code(input).unwrap();
        assert_eq!(code, "4/0AQSTgQ-example");
    }

    #[test]
    fn test_extract_google_authorization_code_from_code_equals_input() {
        let code = extract_google_authorization_code("code=4/0AQSTgQ-inline").unwrap();
        assert_eq!(code, "4/0AQSTgQ-inline");
    }

    #[test]
    fn test_extract_google_authorization_code_rejects_error_redirect() {
        let input = "https://localhost/?error=access_denied&error_description=Denied";
        let err = extract_google_authorization_code(input).unwrap_err();
        assert!(err.contains("authorization failed: access_denied"));
    }

    #[test]
    fn test_parse_google_device_code_response_accepts_verification_uri() {
        let json = r#"{
            "device_code": "device-123",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://www.google.com/device",
            "verification_uri_complete": "https://www.google.com/device?user_code=ABCD-EFGH",
            "expires_in": 1800,
            "interval": 7
        }"#;
        let parsed = parse_google_device_code_response(json).unwrap();
        assert_eq!(parsed.device_code, "device-123");
        assert_eq!(parsed.user_code, "ABCD-EFGH");
        assert_eq!(parsed.verification_uri, "https://www.google.com/device");
        assert_eq!(
            parsed.verification_uri_complete.as_deref(),
            Some("https://www.google.com/device?user_code=ABCD-EFGH")
        );
        assert_eq!(parsed.expires_in_seconds, 1800);
        assert_eq!(parsed.interval_seconds, 7);
    }

    #[test]
    fn test_parse_google_device_code_response_accepts_verification_url_alias() {
        let json = r#"{
            "device_code": "device-abc",
            "user_code": "WXYZ-1234",
            "verification_url": "https://example.com/device"
        }"#;
        let parsed = parse_google_device_code_response(json).unwrap();
        assert_eq!(parsed.verification_uri, "https://example.com/device");
        assert_eq!(parsed.expires_in_seconds, 1800);
        assert_eq!(
            parsed.interval_seconds,
            GOOGLE_DEFAULT_DEVICE_CODE_INTERVAL_SECONDS
        );
    }

    #[test]
    fn test_parse_google_device_code_response_rejects_missing_verification_url() {
        let json = r#"{"device_code":"device-123","user_code":"ABCD-EFGH"}"#;
        assert!(parse_google_device_code_response(json).is_err());
    }

    #[test]
    fn test_is_device_flow_unavailable_error_classifies_unauthorized_client() {
        assert!(is_device_flow_unavailable_error(
            StatusCode::UNAUTHORIZED,
            Some("unauthorized_client"),
            Some("Unauthorized"),
        ));
    }

    #[test]
    fn test_is_device_flow_unavailable_error_does_not_classify_access_denied() {
        assert!(!is_device_flow_unavailable_error(
            StatusCode::BAD_REQUEST,
            Some("access_denied"),
            Some("Denied by user"),
        ));
    }

    #[test]
    fn test_modify_openclaw_config_empty_json() {
        let config = test_config();
        let result = modify_openclaw_config("{}", &config).unwrap();
        let json: Value = serde_json::from_str(&result).unwrap();

        // Check env object
        let env = json["env"].as_object().unwrap();
        assert_eq!(env["CLAWSHELL_API_KEY"], "{clawshell-virtual-key-openai}");

        // Check agents.defaults.models (object map)
        let models = &json["agents"]["defaults"]["models"];
        assert!(models.is_object());
        assert_eq!(models["clawshell/gpt-5.2"]["alias"], "clawshell");

        // Check models.providers (object map)
        let prov = &json["models"]["providers"]["clawshell"];
        assert_eq!(prov["baseUrl"], "http://127.0.0.1:18790/v1");
        assert_eq!(prov["api"], "openai-completions");
        assert_eq!(prov["apiKey"], "${CLAWSHELL_API_KEY}");
        assert_eq!(prov["models"][0]["id"], "gpt-5.2");
        assert_eq!(prov["models"][0]["name"], "gpt-5.2");
    }

    #[test]
    fn test_modify_openclaw_config_preserves_existing_entries() {
        let existing = r#"{
            "env": { "EXISTING_VAR": "value" },
            "agents": {
                "defaults": {
                    "models": {
                        "existing/model": { "alias": "existing" }
                    }
                }
            },
            "models": {
                "providers": {
                    "existing": { "baseUrl": "http://example.com" }
                }
            }
        }"#;

        let config = test_config();
        let result = modify_openclaw_config(existing, &config).unwrap();
        let json: Value = serde_json::from_str(&result).unwrap();

        // Existing env entries preserved, new one added
        let env = json["env"].as_object().unwrap();
        assert_eq!(env.len(), 2);
        assert_eq!(env["EXISTING_VAR"], "value");
        assert_eq!(env["CLAWSHELL_API_KEY"], "{clawshell-virtual-key-openai}");

        // Existing model preserved, new one added
        let models = &json["agents"]["defaults"]["models"];
        assert!(models.is_object());
        assert_eq!(models["existing/model"]["alias"], "existing");
        assert_eq!(models["clawshell/gpt-5.2"]["alias"], "clawshell");

        // Existing provider preserved, new one added
        let providers = &json["models"]["providers"];
        assert!(providers.is_object());
        assert_eq!(providers["existing"]["baseUrl"], "http://example.com");
        assert_eq!(
            providers["clawshell"]["baseUrl"],
            "http://127.0.0.1:18790/v1"
        );
    }

    #[test]
    fn test_modify_openclaw_config_anthropic() {
        let mut config = test_config();
        config.provider = "anthropic".to_string();
        config.model = "claude-sonnet-4-5-20250929".to_string();

        let result = modify_openclaw_config("{}", &config).unwrap();
        let json: Value = serde_json::from_str(&result).unwrap();

        let prov = &json["models"]["providers"]["clawshell"];
        assert_eq!(prov["api"], "openai-completions");
        assert_eq!(prov["models"][0]["id"], "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn test_modify_openclaw_config_invalid_json() {
        let config = test_config();
        let result = modify_openclaw_config("not json", &config);
        assert!(result.is_err());
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

    #[test]
    fn test_default_openclaw_config_path() {
        let path = default_openclaw_config_path();
        assert!(path.contains(".openclaw/openclaw.json"));
    }

    #[test]
    fn test_ensure_nested_object_creates_missing_keys() {
        let mut json = serde_json::json!({});
        ensure_nested_object(&mut json, &["a", "b", "c"]);
        assert!(json["a"]["b"]["c"].is_object());
    }

    #[test]
    fn test_ensure_nested_object_preserves_existing() {
        let mut json = serde_json::json!({"a": {"existing": 42}});
        ensure_nested_object(&mut json, &["a", "b"]);
        assert_eq!(json["a"]["existing"], 42);
        assert!(json["a"]["b"].is_object());
    }

    #[test]
    fn test_is_clawshell_default_model_true() {
        let content = r#"{
            "agents": {
                "defaults": {
                    "model": "clawshell/gpt-5.2"
                }
            }
        }"#;
        assert!(is_clawshell_default_model(content).unwrap());
    }

    #[test]
    fn test_is_clawshell_default_model_false() {
        let content = r#"{
            "agents": {
                "defaults": {
                    "model": "openai/gpt-4o"
                }
            }
        }"#;
        assert!(!is_clawshell_default_model(content).unwrap());
    }

    #[test]
    fn test_is_clawshell_default_model_missing() {
        let content = r#"{
            "agents": {
                "defaults": {}
            }
        }"#;
        assert!(!is_clawshell_default_model(content).unwrap());
    }

    #[test]
    fn test_remove_openclaw_entries() {
        let content = r#"{
            "env": {
                "EXISTING_VAR": "value",
                "CLAWSHELL_API_KEY": "{clawshell-virtual-key-openai}"
            },
            "agents": {
                "defaults": {
                    "models": {
                        "existing/model": { "alias": "existing" },
                        "clawshell/gpt-5.2": { "alias": "clawshell" }
                    }
                }
            },
            "models": {
                "providers": {
                    "existing": { "baseUrl": "http://example.com" },
                    "clawshell": { "baseUrl": "http://127.0.0.1:18790/v1" }
                }
            }
        }"#;

        let result = remove_openclaw_entries(content).unwrap();
        let json: Value = serde_json::from_str(&result).unwrap();

        // env: CLAWSHELL_API_KEY removed, EXISTING_VAR preserved
        let env = json["env"].as_object().unwrap();
        assert_eq!(env.len(), 1);
        assert_eq!(env["EXISTING_VAR"], "value");

        // agents.defaults.models: clawshell/ key removed, existing preserved
        let models = json["agents"]["defaults"]["models"].as_object().unwrap();
        assert_eq!(models.len(), 1);
        assert!(models.contains_key("existing/model"));
        assert!(!models.contains_key("clawshell/gpt-5.2"));

        // models.providers: clawshell removed, existing preserved
        let providers = json["models"]["providers"].as_object().unwrap();
        assert_eq!(providers.len(), 1);
        assert!(providers.contains_key("existing"));
        assert!(!providers.contains_key("clawshell"));
    }

    #[test]
    fn test_detect_keys_from_auth_profiles() {
        let root = VfsPath::new(MemoryFS::new());
        let profiles = serde_json::json!({
            "profiles": {
                "anthropic:default": { "key": "sk-ant-detect-123" },
                "openai:default": { "key": "sk-oai-detect-456" }
            }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/agents/myagent/agent/auth-profiles.json",
            &serde_json::to_string(&profiles).unwrap(),
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-detect-123"));
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-detect-456"));
    }

    #[test]
    fn test_detect_keys_from_dot_env() {
        let root = VfsPath::new(MemoryFS::new());
        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "ANTHROPIC_API_KEY=sk-ant-env-789\nOPENAI_API_KEY=sk-oai-env-012\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-env-789"));
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-env-012"));
    }

    #[test]
    fn test_detect_keys_auth_profiles_takes_priority_over_dot_env() {
        let root = VfsPath::new(MemoryFS::new());

        // auth-profiles has only anthropic
        let profiles = serde_json::json!({
            "profiles": {
                "anthropic:default": { "key": "sk-ant-from-profile" }
            }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/agents/a1/agent/auth-profiles.json",
            &serde_json::to_string(&profiles).unwrap(),
        );

        // .env has both
        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "ANTHROPIC_API_KEY=sk-ant-from-env\nOPENAI_API_KEY=sk-oai-from-env\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        // anthropic from auth-profiles wins
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-from-profile"));
        // openai falls through to .env
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-from-env"));
    }

    #[test]
    fn test_detect_keys_no_state_dir() {
        let root = VfsPath::new(MemoryFS::new());
        // Create a home dir with no .openclaw etc.
        root.join("home/user").unwrap().create_dir_all().unwrap();

        let home = root.join("home/user").unwrap();
        // Should not panic — keys come from env vars (or be None)
        let keys = detect_openclaw_api_keys_vfs(&home);
        let _ = keys;
    }

    #[test]
    fn test_detect_keys_fallback_state_dirs() {
        let root = VfsPath::new(MemoryFS::new());

        // Only .clawdbot exists (second candidate)
        vfs_write(
            &root,
            "home/user/.clawdbot/.env",
            "ANTHROPIC_API_KEY=sk-ant-clawdbot\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-clawdbot"));
    }

    #[test]
    fn test_detect_keys_dot_env_skips_empty_and_comments() {
        let root = VfsPath::new(MemoryFS::new());
        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "# comment\n\nANTHROPIC_API_KEY=\"sk-quoted\"\nOPENAI_API_KEY=\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-quoted"));
        // Empty value should be skipped
        assert!(keys.openai.is_none() || keys.openai.as_deref() != Some(""));
    }

    #[test]
    fn test_existing_config_has_any() {
        let empty = ExistingConfig::default();
        assert!(!empty.has_any());

        let with_provider = ExistingConfig {
            provider: Some("openai".to_string()),
            ..Default::default()
        };
        assert!(with_provider.has_any());

        let with_model = ExistingConfig {
            model: Some("gpt-4".to_string()),
            ..Default::default()
        };
        assert!(with_model.has_any());

        let with_host = ExistingConfig {
            server_host: Some("0.0.0.0".to_string()),
            ..Default::default()
        };
        assert!(with_host.has_any());
    }

    #[test]
    fn test_detected_keys_for_provider() {
        let keys = DetectedKeys {
            anthropic: Some("ant-key".to_string()),
            openai: Some("oai-key".to_string()),
        };
        assert_eq!(keys.for_provider("anthropic"), Some("ant-key"));
        assert_eq!(keys.for_provider("openai"), Some("oai-key"));
        assert_eq!(keys.for_provider("other"), None);

        let empty = DetectedKeys::default();
        assert_eq!(empty.for_provider("anthropic"), None);
    }

    #[test]
    fn test_load_existing_config_from_temp_dir() {
        let root = VfsPath::new(MemoryFS::new());

        // Write config.json
        let config_json = serde_json::json!({
            "provider": "anthropic",
            "model": "claude-sonnet-4-5-20250929",
            "real_api_key": "sk-ant-existing",
            "virtual_api_key": "{clawshell-virtual-key-anthropic}",
            "openclaw_config_path": "/home/user/.openclaw/openclaw.json"
        });
        vfs_write(
            &root,
            "etc/clawshell/config.json",
            &serde_json::to_string_pretty(&config_json).unwrap(),
        );

        // Write clawshell.toml
        vfs_write(
            &root,
            "etc/clawshell/clawshell.toml",
            "[server]\nhost = \"0.0.0.0\"\nport = 9999\n",
        );

        let config_dir = root.join("etc/clawshell").unwrap();
        let existing = load_existing_config_from_vfs(&config_dir).unwrap();
        assert_eq!(existing.provider.as_deref(), Some("anthropic"));
        assert_eq!(
            existing.model.as_deref(),
            Some("claude-sonnet-4-5-20250929")
        );
        assert_eq!(existing.real_api_key.as_deref(), Some("sk-ant-existing"));
        assert_eq!(
            existing.virtual_api_key.as_deref(),
            Some("{clawshell-virtual-key-anthropic}")
        );
        assert_eq!(
            existing.openclaw_config_path.as_deref(),
            Some("/home/user/.openclaw/openclaw.json")
        );
        assert_eq!(existing.server_host.as_deref(), Some("0.0.0.0"));
        assert_eq!(existing.server_port.as_deref(), Some("9999"));
    }

    #[test]
    fn test_load_existing_config_reads_gmail_defaults() {
        let root = VfsPath::new(MemoryFS::new());

        let config_json = serde_json::json!({
            "provider": "openai",
            "model": "gpt-5.2-chat-latest",
            "real_api_key": "sk-existing",
            "virtual_api_key": "{clawshell-virtual-key-openai}",
            "openclaw_config_path": "/home/user/.openclaw/openclaw.json"
        });
        vfs_write(
            &root,
            "etc/clawshell/config.json",
            &serde_json::to_string_pretty(&config_json).unwrap(),
        );

        vfs_write(
            &root,
            "etc/clawshell/clawshell.toml",
            r#"[server]
host = "127.0.0.1"
port = 18790

[gmail]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com", "@trusted.org"]

[[gmail.accounts]]
virtual_key = "{clawshell-virtual-key-openai}"
refresh_token = "refresh-existing"
client_secret_file = "/etc/clawshell/oauth/client_secret.json"
user_id = "me"
"#,
        );

        let config_dir = root.join("etc/clawshell").unwrap();
        let existing = load_existing_config_from_vfs(&config_dir).unwrap();
        assert_eq!(existing.gmail_enabled, Some(true));
        assert_eq!(existing.gmail_mode, Some(OnboardGmailMode::Allowlist));
        assert_eq!(
            existing.gmail_sender_rules,
            vec!["alice@example.com".to_string(), "@trusted.org".to_string()]
        );
        assert_eq!(
            existing.gmail_account_virtual_key.as_deref(),
            Some("{clawshell-virtual-key-openai}")
        );
        assert_eq!(
            existing.gmail_refresh_token.as_deref(),
            Some("refresh-existing")
        );
        assert_eq!(
            existing.gmail_client_secret_file.as_deref(),
            Some("/etc/clawshell/oauth/client_secret.json")
        );
    }

    #[test]
    fn test_load_existing_config_prefers_matching_gmail_account() {
        let root = VfsPath::new(MemoryFS::new());

        let config_json = serde_json::json!({
            "provider": "openai",
            "model": "gpt-5.2-chat-latest",
            "real_api_key": "sk-existing",
            "virtual_api_key": "vk-match",
            "openclaw_config_path": "/home/user/.openclaw/openclaw.json"
        });
        vfs_write(
            &root,
            "etc/clawshell/config.json",
            &serde_json::to_string_pretty(&config_json).unwrap(),
        );

        vfs_write(
            &root,
            "etc/clawshell/clawshell.toml",
            r#"[server]
host = "127.0.0.1"
port = 18790

[gmail]
enabled = true
mode = "denylist"
deny_senders = ["@blocked.com"]

[[gmail.accounts]]
virtual_key = "vk-other"
refresh_token = "refresh-other"
client_secret_file = "/etc/clawshell/oauth/other_client_secret.json"
user_id = "me"

[[gmail.accounts]]
virtual_key = "vk-match"
refresh_token = "refresh-match"
client_secret_file = "/etc/clawshell/oauth/match_client_secret.json"
user_id = "me"
"#,
        );

        let config_dir = root.join("etc/clawshell").unwrap();
        let existing = load_existing_config_from_vfs(&config_dir).unwrap();
        assert_eq!(existing.gmail_mode, Some(OnboardGmailMode::Denylist));
        assert_eq!(
            existing.gmail_sender_rules,
            vec!["@blocked.com".to_string()]
        );
        assert_eq!(
            existing.gmail_account_virtual_key.as_deref(),
            Some("vk-match")
        );
        assert_eq!(
            existing.gmail_refresh_token.as_deref(),
            Some("refresh-match")
        );
        assert_eq!(
            existing.gmail_client_secret_file.as_deref(),
            Some("/etc/clawshell/oauth/match_client_secret.json")
        );
    }

    #[test]
    fn test_load_existing_config_from_empty_dir() {
        let root = VfsPath::new(MemoryFS::new());
        root.join("etc/clawshell")
            .unwrap()
            .create_dir_all()
            .unwrap();

        let config_dir = root.join("etc/clawshell").unwrap();
        let result = load_existing_config_from_vfs(&config_dir);
        assert!(result.is_none());
    }

    #[test]
    fn test_load_existing_config_from_partial() {
        let root = VfsPath::new(MemoryFS::new());

        // Only clawshell.toml, no config.json
        vfs_write(
            &root,
            "etc/clawshell/clawshell.toml",
            "[server]\nhost = \"127.0.0.1\"\nport = 18790\n",
        );

        let config_dir = root.join("etc/clawshell").unwrap();
        let existing = load_existing_config_from_vfs(&config_dir).unwrap();
        assert!(existing.provider.is_none());
        assert_eq!(existing.server_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(existing.server_port.as_deref(), Some("18790"));
    }

    #[test]
    fn test_remove_openclaw_entries_preserves_other() {
        let content = r#"{
            "env": {
                "MY_VAR": "abc",
                "OTHER_VAR": "def"
            },
            "agents": {
                "defaults": {
                    "models": {
                        "openai/gpt-4o": { "alias": "openai" }
                    }
                }
            },
            "models": {
                "providers": {
                    "openai": { "baseUrl": "https://api.openai.com" }
                }
            },
            "extra_field": 42
        }"#;

        let result = remove_openclaw_entries(content).unwrap();
        let json: Value = serde_json::from_str(&result).unwrap();

        // Everything should be preserved since there are no clawshell entries
        let env = json["env"].as_object().unwrap();
        assert_eq!(env.len(), 2);

        let models = json["agents"]["defaults"]["models"].as_object().unwrap();
        assert_eq!(models.len(), 1);
        assert!(models.contains_key("openai/gpt-4o"));

        let providers = json["models"]["providers"].as_object().unwrap();
        assert_eq!(providers.len(), 1);
        assert!(providers.contains_key("openai"));

        assert_eq!(json["extra_field"], 42);
    }

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
