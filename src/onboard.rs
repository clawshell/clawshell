use crate::platform;
use crate::tui;

use serde_json::Value;
use std::path::{Path, PathBuf};
use tracing::warn;
use vfs::VfsPath;

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

    // Read clawshell.toml for server host/port
    if let Ok(content) = toml_file.read_to_string()
        && let Ok(toml) = content.parse::<toml::Table>()
        && let Some(server) = toml.get("server").and_then(|s| s.as_table())
    {
        existing.server_host = server
            .get("host")
            .and_then(|v| v.as_str())
            .map(String::from);
        existing.server_port = server
            .get("port")
            .and_then(|v| v.as_integer())
            .map(|p| p.to_string());
    }

    if existing.has_any() {
        Some(existing)
    } else {
        None
    }
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
        let masked = if existing_key.len() > 8 {
            format!(
                "{}...{}",
                &existing_key[..4],
                &existing_key[existing_key.len() - 4..]
            )
        } else {
            "*".repeat(existing_key.len())
        };
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
    })
}

/// Generate the ClawShell TOML configuration content with the given key mapping.
pub fn generate_clawshell_config(config: &OnboardConfig) -> String {
    format!(
        r#"# ClawShell Configuration
log_level = "info"

[server]
host = "{host}"
port = {port}

[upstream]
base_url = "https://api.openai.com"
anthropic_base_url = "https://api.anthropic.com"

[[keys]]
virtual_key = "{virtual_key}"
real_key = "{real_key}"
provider = "{provider}"
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
        host = config.server_host,
        port = config.server_port,
        virtual_key = config.virtual_api_key,
        real_key = config.real_api_key,
        provider = config.provider,
    )
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
        assert!(toml_str.contains("[dlp]"));
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
