use crate::tui;

use serde_json::Value;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

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
    let mut keys = DetectedKeys::default();

    // Find the state directory
    let state_dir = match home.and_then(find_state_dir) {
        Some(d) => d,
        None => {
            // Fall back to env vars only
            keys.anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
            keys.openai = std::env::var("OPENAI_API_KEY").ok();
            return keys;
        }
    };

    // Strategy 1: auth-profiles.json
    try_auth_profiles(&state_dir, &mut keys);

    // Strategy 2: .env file
    if keys.anthropic.is_none() || keys.openai.is_none() {
        try_dot_env(&state_dir, &mut keys);
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

/// Find the first existing OpenClaw state directory.
fn find_state_dir(home: &str) -> Option<PathBuf> {
    let candidates = [".openclaw", ".clawdbot", ".moltbot", ".moldbot"];
    for name in &candidates {
        let path = PathBuf::from(home).join(name);
        if path.is_dir() {
            return Some(path);
        }
    }
    None
}

/// Scan auth-profiles.json files for API keys.
fn try_auth_profiles(state_dir: &Path, keys: &mut DetectedKeys) {
    let agents_dir = state_dir.join("agents");
    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let profile_path = entry.path().join("agent").join("auth-profiles.json");
        if let Ok(content) = std::fs::read_to_string(&profile_path)
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

/// Parse a .env file for API keys.
fn try_dot_env(state_dir: &Path, keys: &mut DetectedKeys) {
    let env_path = state_dir.join(".env");
    let content = match std::fs::read_to_string(&env_path) {
        Ok(c) => c,
        Err(_) => return,
    };
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

/// Prompt the user for input with a message. Returns trimmed input.
pub fn prompt(reader: &mut dyn BufRead, writer: &mut dyn Write, msg: &str) -> io::Result<String> {
    write!(writer, "{}", msg)?;
    writer.flush()?;
    let mut input = String::new();
    reader.read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// Prompt the user with a default value. Empty input returns the default.
pub fn prompt_with_default(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
    msg: &str,
    default: &str,
) -> io::Result<String> {
    write!(writer, "{} [{}]: ", msg, default)?;
    writer.flush()?;
    let mut input = String::new();
    reader.read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

/// Prompt the user to choose a provider (openai or anthropic).
pub fn prompt_provider(reader: &mut dyn BufRead, writer: &mut dyn Write) -> io::Result<String> {
    writeln!(writer, "Select a model provider:")?;
    writeln!(writer, "  1) OpenAI")?;
    writeln!(writer, "  2) Anthropic")?;
    let choice = prompt(reader, writer, "Enter choice (1 or 2): ")?;
    match choice.as_str() {
        "1" | "openai" | "OpenAI" => Ok("openai".to_string()),
        "2" | "anthropic" | "Anthropic" => Ok("anthropic".to_string()),
        _ => {
            writeln!(
                writer,
                "Invalid choice '{}', defaulting to 'openai'.",
                choice
            )?;
            Ok("openai".to_string())
        }
    }
}

/// Collect all onboarding information from the user interactively.
pub fn collect_onboard_config(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
) -> io::Result<OnboardConfig> {
    collect_onboard_config_with_detected(reader, writer, detect_openclaw_api_keys())
}

fn collect_onboard_config_with_detected(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
    detected: DetectedKeys,
) -> io::Result<OnboardConfig> {
    writeln!(writer)?;
    writeln!(writer, "--- API Configuration ---")?;
    writeln!(writer)?;

    // Provider
    let provider = prompt_provider(reader, writer)?;

    // Model
    let default_model = if provider == "anthropic" {
        "claude-sonnet-4-5-20250929"
    } else {
        "gpt-5.2-chat-latest"
    };
    let model = prompt_with_default(reader, writer, "Enter the model name", default_model)?;

    // Real API key — try to detect from OpenClaw installation
    let real_api_key = if let Some(detected_key) = detected.for_provider(&provider) {
        writeln!(
            writer,
            "An API key was detected from your OpenClaw config. \
             It is strongly recommended to generate a new key from your provider, \
             enter it here instead, and revoke the old one."
        )?;
        prompt_with_default(
            reader,
            writer,
            "Enter the real API key for the selected provider",
            detected_key,
        )?
    } else {
        prompt(
            reader,
            writer,
            "Enter the real API key for the selected provider: ",
        )?
    };
    if real_api_key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "API key cannot be empty",
        ));
    }

    // Virtual API key
    let default_virtual_key = format!("{{clawshell-virtual-key-{}}}", provider);
    let virtual_api_key = prompt_with_default(
        reader,
        writer,
        "Enter the virtual API key for OpenClaw",
        &default_virtual_key,
    )?;

    writeln!(writer)?;
    writeln!(writer, "--- OpenClaw Configuration ---")?;
    writeln!(writer)?;

    // OpenClaw config path
    let default_openclaw_path = default_openclaw_config_path();
    let openclaw_config_path = prompt_with_default(
        reader,
        writer,
        "Enter the OpenClaw configuration file path",
        &default_openclaw_path,
    )?;

    // Server settings
    let server_host =
        prompt_with_default(reader, writer, "Enter the ClawShell server IP", "127.0.0.1")?;
    let server_port_str =
        prompt_with_default(reader, writer, "Enter the ClawShell server port", "18790")?;
    let server_port: u16 = server_port_str
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid port number"))?;

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
    load_existing_config_from(&PathBuf::from("/etc/clawshell"))
}

/// Inner implementation that accepts an explicit config directory for testability.
fn load_existing_config_from(config_dir: &Path) -> Option<ExistingConfig> {
    let config_file = config_dir.join("config.json");
    let toml_file = config_dir.join("clawshell.toml");

    let mut existing = ExistingConfig::default();

    // Read config.json for provider, model, virtual_api_key, openclaw_config_path
    if let Ok(content) = std::fs::read_to_string(&config_file)
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
    if let Ok(content) = std::fs::read_to_string(&toml_file)
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

/// Backup the OpenClaw configuration file.
/// Returns the backup path on success.
pub fn backup_openclaw_config(openclaw_path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if !openclaw_path.exists() {
        return Err(format!(
            "OpenClaw configuration file not found at: {}",
            openclaw_path.display()
        )
        .into());
    }

    let base_backup = openclaw_path.with_file_name("openclaw.json.clawshell.bak");
    let backup_path = if base_backup.exists() {
        // Find the next available numbered backup
        let mut n = 1u32;
        loop {
            let numbered = openclaw_path.with_file_name(format!("openclaw.json.clawshell.bak.{n}"));
            if !numbered.exists() {
                break numbered;
            }
            n += 1;
        }
    } else {
        base_backup
    };
    std::fs::copy(openclaw_path, &backup_path)?;

    // Lock down the backup so no user can read it (contains sensitive config).
    // Restore requires `sudo chmod 600` first.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o000))?;

    // Chown the backup to the clawshell user
    let chown_spec = if cfg!(target_os = "macos") {
        "clawshell:staff"
    } else {
        "clawshell:clawshell"
    };
    let _ = std::process::Command::new("chown")
        .args([chown_spec, &backup_path.to_string_lossy()])
        .status();

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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

    #[test]
    fn test_prompt_reads_input() {
        let input = b"hello world\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt(&mut reader, &mut output, "Enter: ").unwrap();
        assert_eq!(result, "hello world");
        assert_eq!(String::from_utf8_lossy(&output), "Enter: ");
    }

    #[test]
    fn test_prompt_trims_whitespace() {
        let input = b"  spaced  \n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt(&mut reader, &mut output, "> ").unwrap();
        assert_eq!(result, "spaced");
    }

    #[test]
    fn test_prompt_with_default_uses_default_on_empty() {
        let input = b"\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt_with_default(&mut reader, &mut output, "Port", "18790").unwrap();
        assert_eq!(result, "18790");
    }

    #[test]
    fn test_prompt_with_default_uses_input_when_provided() {
        let input = b"8080\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt_with_default(&mut reader, &mut output, "Port", "18790").unwrap();
        assert_eq!(result, "8080");
    }

    #[test]
    fn test_prompt_provider_openai() {
        let input = b"1\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt_provider(&mut reader, &mut output).unwrap();
        assert_eq!(result, "openai");
    }

    #[test]
    fn test_prompt_provider_anthropic() {
        let input = b"2\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt_provider(&mut reader, &mut output).unwrap();
        assert_eq!(result, "anthropic");
    }

    #[test]
    fn test_prompt_provider_by_name() {
        let input = b"anthropic\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt_provider(&mut reader, &mut output).unwrap();
        assert_eq!(result, "anthropic");
    }

    #[test]
    fn test_prompt_provider_invalid_defaults_to_openai() {
        let input = b"xyz\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result = prompt_provider(&mut reader, &mut output).unwrap();
        assert_eq!(result, "openai");
    }

    #[test]
    fn test_collect_onboard_config_openai() {
        let input = b"1\n\nsk-test-key\n\n\n\n\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let config =
            collect_onboard_config_with_detected(&mut reader, &mut output, DetectedKeys::default())
                .unwrap();
        assert_eq!(config.provider, "openai");
        assert_eq!(config.model, "gpt-5.2-chat-latest");
        assert_eq!(config.real_api_key, "sk-test-key");
        assert_eq!(config.virtual_api_key, "{clawshell-virtual-key-openai}");
        assert_eq!(config.server_host, "127.0.0.1");
        assert_eq!(config.server_port, 18790);
    }

    #[test]
    fn test_collect_onboard_config_anthropic() {
        let input = b"2\nclaude-opus-4-6\nsk-ant-test\nmy-virtual-key\n/custom/path.json\n192.168.1.1\n8080\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let config =
            collect_onboard_config_with_detected(&mut reader, &mut output, DetectedKeys::default())
                .unwrap();
        assert_eq!(config.provider, "anthropic");
        assert_eq!(config.model, "claude-opus-4-6");
        assert_eq!(config.real_api_key, "sk-ant-test");
        assert_eq!(config.virtual_api_key, "my-virtual-key");
        assert_eq!(
            config.openclaw_config_path,
            PathBuf::from("/custom/path.json")
        );
        assert_eq!(config.server_host, "192.168.1.1");
        assert_eq!(config.server_port, 8080);
    }

    #[test]
    fn test_collect_onboard_config_empty_api_key_fails() {
        let input = b"1\n\n\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let result =
            collect_onboard_config_with_detected(&mut reader, &mut output, DetectedKeys::default());
        assert!(result.is_err());
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
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join("clawshell_test_backup");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("openclaw.json");
        std::fs::write(&config_path, r#"{"test": true}"#).unwrap();

        let backup_path = backup_openclaw_config(&config_path).unwrap();
        assert_eq!(backup_path, dir.join("openclaw.json.clawshell.bak"));
        assert!(backup_path.exists());

        // Verify backup is locked down (mode 000)
        let perms = std::fs::metadata(&backup_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o000);

        // Restore read permission to verify content, then clean up
        std::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let backup_content = std::fs::read_to_string(&backup_path).unwrap();
        assert_eq!(backup_content, r#"{"test": true}"#);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_backup_openclaw_config_numbered() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join("clawshell_test_backup_numbered");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("openclaw.json");

        // First backup: creates .bak
        std::fs::write(&config_path, r#"{"v": 0}"#).unwrap();
        let bak0 = backup_openclaw_config(&config_path).unwrap();
        assert_eq!(bak0, dir.join("openclaw.json.clawshell.bak"));

        // Second backup: .bak exists, creates .bak.1
        std::fs::write(&config_path, r#"{"v": 1}"#).unwrap();
        let bak1 = backup_openclaw_config(&config_path).unwrap();
        assert_eq!(bak1, dir.join("openclaw.json.clawshell.bak.1"));

        // Third backup: .bak and .bak.1 exist, creates .bak.2
        std::fs::write(&config_path, r#"{"v": 2}"#).unwrap();
        let bak2 = backup_openclaw_config(&config_path).unwrap();
        assert_eq!(bak2, dir.join("openclaw.json.clawshell.bak.2"));

        // Verify contents
        std::fs::set_permissions(&bak0, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&bak1, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&bak2, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read_to_string(&bak0).unwrap(), r#"{"v": 0}"#);
        assert_eq!(std::fs::read_to_string(&bak1).unwrap(), r#"{"v": 1}"#);
        assert_eq!(std::fs::read_to_string(&bak2).unwrap(), r#"{"v": 2}"#);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_backup_openclaw_config_missing_file() {
        let result = backup_openclaw_config(Path::new("/nonexistent/openclaw.json"));
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
        let dir = std::env::temp_dir().join("clawshell_test_detect_auth");
        let _ = std::fs::remove_dir_all(&dir);
        let state_dir = dir.join(".openclaw");
        let agent_dir = state_dir.join("agents").join("myagent").join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let profiles = serde_json::json!({
            "profiles": {
                "anthropic:default": { "key": "sk-ant-detect-123" },
                "openai:default": { "key": "sk-oai-detect-456" }
            }
        });
        std::fs::write(
            agent_dir.join("auth-profiles.json"),
            serde_json::to_string(&profiles).unwrap(),
        )
        .unwrap();

        let keys = detect_openclaw_api_keys_with_home(Some(dir.to_str().unwrap()));
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-detect-123"));
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-detect-456"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_keys_from_dot_env() {
        let dir = std::env::temp_dir().join("clawshell_test_detect_dotenv");
        let _ = std::fs::remove_dir_all(&dir);
        let state_dir = dir.join(".openclaw");
        std::fs::create_dir_all(&state_dir).unwrap();

        std::fs::write(
            state_dir.join(".env"),
            "ANTHROPIC_API_KEY=sk-ant-env-789\nOPENAI_API_KEY=sk-oai-env-012\n",
        )
        .unwrap();

        let keys = detect_openclaw_api_keys_with_home(Some(dir.to_str().unwrap()));
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-env-789"));
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-env-012"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_keys_auth_profiles_takes_priority_over_dot_env() {
        let dir = std::env::temp_dir().join("clawshell_test_detect_priority");
        let _ = std::fs::remove_dir_all(&dir);
        let state_dir = dir.join(".openclaw");
        let agent_dir = state_dir.join("agents").join("a1").join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // auth-profiles has only anthropic
        let profiles = serde_json::json!({
            "profiles": {
                "anthropic:default": { "key": "sk-ant-from-profile" }
            }
        });
        std::fs::write(
            agent_dir.join("auth-profiles.json"),
            serde_json::to_string(&profiles).unwrap(),
        )
        .unwrap();

        // .env has both
        std::fs::write(
            state_dir.join(".env"),
            "ANTHROPIC_API_KEY=sk-ant-from-env\nOPENAI_API_KEY=sk-oai-from-env\n",
        )
        .unwrap();

        let keys = detect_openclaw_api_keys_with_home(Some(dir.to_str().unwrap()));
        // anthropic from auth-profiles wins
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-from-profile"));
        // openai falls through to .env
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-from-env"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_keys_no_state_dir() {
        let dir = std::env::temp_dir().join("clawshell_test_detect_none");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No .openclaw etc. directories exist
        let keys = detect_openclaw_api_keys_with_home(Some(dir.to_str().unwrap()));
        // Without env vars set for this test, should be None
        // (env vars may or may not be set in the test environment, so we just
        // verify the function doesn't panic)
        let _ = keys;

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_keys_fallback_state_dirs() {
        let dir = std::env::temp_dir().join("clawshell_test_detect_fallback");
        let _ = std::fs::remove_dir_all(&dir);

        // Only .clawdbot exists (second candidate)
        let state_dir = dir.join(".clawdbot");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            state_dir.join(".env"),
            "ANTHROPIC_API_KEY=sk-ant-clawdbot\n",
        )
        .unwrap();

        let keys = detect_openclaw_api_keys_with_home(Some(dir.to_str().unwrap()));
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-clawdbot"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_keys_dot_env_skips_empty_and_comments() {
        let dir = std::env::temp_dir().join("clawshell_test_detect_env_parse");
        let _ = std::fs::remove_dir_all(&dir);
        let state_dir = dir.join(".openclaw");
        std::fs::create_dir_all(&state_dir).unwrap();

        std::fs::write(
            state_dir.join(".env"),
            "# comment\n\nANTHROPIC_API_KEY=\"sk-quoted\"\nOPENAI_API_KEY=\n",
        )
        .unwrap();

        let keys = detect_openclaw_api_keys_with_home(Some(dir.to_str().unwrap()));
        assert_eq!(keys.anthropic.as_deref(), Some("sk-quoted"));
        // Empty value should be skipped
        assert!(keys.openai.is_none() || keys.openai.as_deref() != Some(""));

        let _ = std::fs::remove_dir_all(&dir);
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
    fn test_collect_onboard_config_with_detected_key() {
        // Simulate providing input where the detected key is offered as a default
        // but the user enters a new key
        let input = b"1\ngpt-5.2\nnew-key-123\nvk-test\n/tmp/oc.json\n127.0.0.1\n18790\n";
        let mut reader = Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let detected = DetectedKeys {
            openai: Some("old-detected-key".to_string()),
            ..DetectedKeys::default()
        };
        let result = collect_onboard_config_with_detected(&mut reader, &mut output, detected);
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.real_api_key, "new-key-123");
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
        let dir = std::env::temp_dir().join("clawshell_test_load_existing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Write config.json
        let config_json = serde_json::json!({
            "provider": "anthropic",
            "model": "claude-sonnet-4-5-20250929",
            "real_api_key": "sk-ant-existing",
            "virtual_api_key": "{clawshell-virtual-key-anthropic}",
            "openclaw_config_path": "/home/user/.openclaw/openclaw.json"
        });
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string_pretty(&config_json).unwrap(),
        )
        .unwrap();

        // Write clawshell.toml
        std::fs::write(
            dir.join("clawshell.toml"),
            "[server]\nhost = \"0.0.0.0\"\nport = 9999\n",
        )
        .unwrap();

        let existing = load_existing_config_from(&dir).unwrap();
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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_existing_config_from_empty_dir() {
        let dir = std::env::temp_dir().join("clawshell_test_load_existing_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let result = load_existing_config_from(&dir);
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_existing_config_from_partial() {
        let dir = std::env::temp_dir().join("clawshell_test_load_existing_partial");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Only clawshell.toml, no config.json
        std::fs::write(
            dir.join("clawshell.toml"),
            "[server]\nhost = \"127.0.0.1\"\nport = 18790\n",
        )
        .unwrap();

        let existing = load_existing_config_from(&dir).unwrap();
        assert!(existing.provider.is_none());
        assert_eq!(existing.server_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(existing.server_port.as_deref(), Some("18790"));

        let _ = std::fs::remove_dir_all(&dir);
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
}
