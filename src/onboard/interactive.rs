use super::config_render::default_openclaw_config_path;
use super::credentials::detect_openclaw_api_key_for_provider;
use super::types::{OnboardAuthMethod, OnboardConfig, OnboardEmailConfig, OnboardEmailMode};
use crate::email::{EmailAccountCredentials, ImapEmailService};
use crate::tui;

use std::path::PathBuf;
use std::time::Duration;
use vfs::VfsPath;

const EMAIL_PROVIDER_PRESET_GMAIL: &str = "Gmail (imap.gmail.com:993)";
const EMAIL_PROVIDER_PRESET_OUTLOOK: &str = "Outlook (imap-mail.outlook.com:993)";
const EMAIL_PROVIDER_PRESET_OTHER: &str = "Other (manual IMAP host/port)";
const EMAIL_DEFAULT_GMAIL_IMAP_HOST: &str = "imap.gmail.com";
const EMAIL_DEFAULT_OUTLOOK_IMAP_HOST: &str = "imap-mail.outlook.com";
const EMAIL_DEFAULT_IMAP_PORT: u16 = 993;
const EMAIL_IMAP_VALIDATE_TIMEOUT_SECONDS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmailImapProviderPreset {
    Gmail,
    Outlook,
    Other,
}

impl EmailImapProviderPreset {
    fn label(self) -> &'static str {
        match self {
            EmailImapProviderPreset::Gmail => EMAIL_PROVIDER_PRESET_GMAIL,
            EmailImapProviderPreset::Outlook => EMAIL_PROVIDER_PRESET_OUTLOOK,
            EmailImapProviderPreset::Other => EMAIL_PROVIDER_PRESET_OTHER,
        }
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
        existing.auth_method = json
            .get("auth_method")
            .and_then(|v| v.as_str())
            .map(String::from);
        existing.oauth_provider = json
            .get("oauth_provider")
            .and_then(|v| v.as_str())
            .map(String::from);
    }

    // Read clawshell.toml for server host/port and optional Email settings
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

        if let Some(email) = toml.get("email").and_then(|g| g.as_table()) {
            existing.email_enabled = email.get("enabled").and_then(|v| v.as_bool());
            existing.email_mode = email
                .get("mode")
                .and_then(|v| v.as_str())
                .and_then(parse_email_mode);

            let allow_rules = email
                .get("allow_senders")
                .and_then(|v| v.as_array())
                .map(|values| parse_string_array(values))
                .unwrap_or_default();
            let deny_rules = email
                .get("deny_senders")
                .and_then(|v| v.as_array())
                .map(|values| parse_string_array(values))
                .unwrap_or_default();
            existing.email_sender_rules = if !allow_rules.is_empty() {
                allow_rules
            } else {
                deny_rules
            };

            if let Some(account) = select_existing_email_account(
                email
                    .get("accounts")
                    .and_then(|v| v.as_array().map(Vec::as_slice)),
                existing.virtual_api_key.as_deref(),
            ) {
                existing.email_account_virtual_key = account
                    .get("virtual_key")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                existing.email_email = account
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                existing.email_app_password = account
                    .get("app_password")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                existing.email_imap_host = account
                    .get("imap_host")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                existing.email_imap_port = account
                    .get("imap_port")
                    .and_then(|v| v.as_integer())
                    .and_then(|value| u16::try_from(value).ok());
            }
        }
    }

    if existing.has_any() {
        Some(existing)
    } else {
        None
    }
}

fn parse_email_mode(value: &str) -> Option<OnboardEmailMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "allowlist" => Some(OnboardEmailMode::Allowlist),
        "denylist" => Some(OnboardEmailMode::Denylist),
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

fn infer_imap_provider_preset(imap_host: Option<&str>) -> EmailImapProviderPreset {
    let Some(host) = imap_host else {
        return EmailImapProviderPreset::Gmail;
    };

    match host.trim().to_ascii_lowercase().as_str() {
        EMAIL_DEFAULT_GMAIL_IMAP_HOST => EmailImapProviderPreset::Gmail,
        EMAIL_DEFAULT_OUTLOOK_IMAP_HOST => EmailImapProviderPreset::Outlook,
        _ => EmailImapProviderPreset::Other,
    }
}

fn email_imap_provider_options(preferred: EmailImapProviderPreset) -> Vec<&'static str> {
    let mut options = vec![preferred.label()];
    for provider in [
        EmailImapProviderPreset::Gmail,
        EmailImapProviderPreset::Outlook,
        EmailImapProviderPreset::Other,
    ] {
        if provider != preferred {
            options.push(provider.label());
        }
    }
    options
}

fn parse_imap_provider_choice(choice: &str) -> EmailImapProviderPreset {
    match choice {
        EMAIL_PROVIDER_PRESET_GMAIL => EmailImapProviderPreset::Gmail,
        EMAIL_PROVIDER_PRESET_OUTLOOK => EmailImapProviderPreset::Outlook,
        EMAIL_PROVIDER_PRESET_OTHER => EmailImapProviderPreset::Other,
        _ => EmailImapProviderPreset::Gmail,
    }
}

fn email_provider_defaults(provider: EmailImapProviderPreset) -> Option<(&'static str, u16)> {
    match provider {
        EmailImapProviderPreset::Gmail => {
            Some((EMAIL_DEFAULT_GMAIL_IMAP_HOST, EMAIL_DEFAULT_IMAP_PORT))
        }
        EmailImapProviderPreset::Outlook => {
            Some((EMAIL_DEFAULT_OUTLOOK_IMAP_HOST, EMAIL_DEFAULT_IMAP_PORT))
        }
        EmailImapProviderPreset::Other => None,
    }
}

fn validate_email_imap_credentials(
    email: &str,
    app_password: &str,
    imap_host: &str,
    imap_port: u16,
) -> Result<(), String> {
    let credentials = EmailAccountCredentials {
        email: email.trim().to_string(),
        app_password: app_password.trim().to_string(),
        imap_host: imap_host.trim().to_string(),
        imap_port,
    };

    ImapEmailService::validate_credentials_blocking(
        &credentials,
        Duration::from_secs(EMAIL_IMAP_VALIDATE_TIMEOUT_SECONDS),
    )
    .map_err(|error| error.to_string())
}

fn select_existing_email_account<'a>(
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
    auth_method: Option<String>,
    oauth_provider: Option<String>,
    email_enabled: Option<bool>,
    email_mode: Option<OnboardEmailMode>,
    email_sender_rules: Vec<String>,
    email_account_virtual_key: Option<String>,
    email_email: Option<String>,
    email_app_password: Option<String>,
    email_imap_host: Option<String>,
    email_imap_port: Option<u16>,
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
            || self.auth_method.is_some()
            || self.oauth_provider.is_some()
            || self.email_enabled.is_some()
            || self.email_mode.is_some()
            || !self.email_sender_rules.is_empty()
            || self.email_account_virtual_key.is_some()
            || self.email_email.is_some()
            || self.email_app_password.is_some()
            || self.email_imap_host.is_some()
            || self.email_imap_port.is_some()
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

fn mask_secret(secret: &str) -> String {
    if secret.len() > 8 {
        format!("{}...{}", &secret[..4], &secret[secret.len() - 4..])
    } else if secret.is_empty() {
        "(empty)".to_string()
    } else {
        "*".repeat(secret.len())
    }
}

/// Run the OAuth login flow for the given provider, persisting tokens.
fn run_oauth_login(provider_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::oauth::codex::CodexProvider;
    use crate::oauth::{OAuthProvider, TokenStorage};

    let provider: Box<dyn OAuthProvider + Send + Sync> = match provider_id {
        "codex" => Box::new(CodexProvider::new(None, None, None, None)),
        other => return Err(format!("unknown OAuth provider: {other}").into()),
    };

    let storage = TokenStorage::default();

    // Called from within #[tokio::main], so use block_in_place to avoid
    // "Cannot start a runtime from within a runtime" panic.
    let run_async = |fut: std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>| {
        let handle = tokio::runtime::Handle::current();
        tokio::task::block_in_place(|| handle.block_on(fut))
    };

    let tokens = if provider.supports_device_code() {
        tui::print_info("Flow", "device code (no browser required)");
        run_async(Box::pin(provider.login_headless()))?
    } else if provider.supports_headless_url() {
        tui::print_info("Flow", "headless (copy URL, paste code)");
        run_async(Box::pin(provider.login_headless()))?
    } else {
        tui::print_info("Flow", "browser login");
        tui::print_warning("A browser window will open for you to authorize access.");
        run_async(Box::pin(provider.login_browser(8400)))?
    };

    storage.save(provider_id, &tokens)?;
    tui::print_success("OAuth login successful — tokens saved.");
    if let Some(acct) = tokens.account_id.as_deref() {
        tui::print_info("Account", acct);
    }

    Ok(())
}

/// Collect a static API key from the user (original flow).
fn collect_static_api_key(
    provider: &str,
    existing: &ExistingConfig,
) -> Result<String, Box<dyn std::error::Error>> {
    let is_first_onboard = existing.real_api_key.is_none();
    let effective_existing_key = if !is_first_onboard {
        existing.real_api_key.clone()
    } else {
        let key = detect_openclaw_api_key_for_provider(provider);
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
        let masked = mask_secret(existing_key);
        tui::print_info("Existing key", &masked);

        let prompt_msg = if is_first_onboard {
            "Enter a NEW API key (recommended) or leave blank to reuse the detected key"
        } else {
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

    Ok(real_api_key)
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

    // Provider selection
    const MENU_OPENAI: &str = "OpenAI";
    const MENU_OPENROUTER: &str = "OpenRouter";
    const MENU_ANTHROPIC: &str = "Anthropic";
    const MENU_CODEX: &str = "Codex / ChatGPT (OAuth)";
    let all_options = [MENU_OPENAI, MENU_OPENROUTER, MENU_ANTHROPIC, MENU_CODEX];

    // Reorder so the existing choice appears first
    let preferred = match (
        existing.auth_method.as_deref(),
        existing.oauth_provider.as_deref(),
        existing.provider.as_deref(),
    ) {
        (Some("oauth"), Some("codex"), _) | (Some("oauth"), _, _) => Some(MENU_CODEX),
        (_, _, Some("anthropic")) => Some(MENU_ANTHROPIC),
        (_, _, Some("openrouter")) => Some(MENU_OPENROUTER),
        (_, _, Some("openai")) => Some(MENU_OPENAI),
        _ => None,
    };
    let provider_options: Vec<&str> = if let Some(first) = preferred {
        std::iter::once(first)
            .chain(all_options.iter().copied().filter(|o| *o != first))
            .collect()
    } else {
        all_options.to_vec()
    };

    let provider_choice = tui::prompt_select("Select a model provider", provider_options)?;

    let (provider, auth_method) = match provider_choice {
        MENU_ANTHROPIC => ("anthropic".to_string(), OnboardAuthMethod::StaticKey),
        MENU_OPENROUTER => ("openrouter".to_string(), OnboardAuthMethod::StaticKey),
        MENU_CODEX => (
            "openai".to_string(),
            OnboardAuthMethod::OAuth {
                provider_id: "codex".to_string(),
            },
        ),
        _ => ("openai".to_string(), OnboardAuthMethod::StaticKey),
    };

    // Model name — use existing model or provider/auth-specific default
    let default_model = existing.model.as_deref().unwrap_or(match provider_choice {
        MENU_ANTHROPIC => "claude-sonnet-4-5-20250929",
        MENU_OPENROUTER => "openrouter/auto",
        MENU_CODEX => "gpt-5.2-chat-latest",
        _ => "gpt-5.2-chat-latest", // OpenAI default
    });
    let model = tui::prompt_text("Enter the model name", Some(default_model))?;

    let real_api_key = match &auth_method {
        OnboardAuthMethod::OAuth { provider_id } => {
            // OAuth flow — run device code or browser login
            tui::print_section("OAuth Login");
            tui::print_info("OAuth provider", provider_id);

            run_oauth_login(provider_id)?;

            // No static API key needed for OAuth
            String::new()
        }
        OnboardAuthMethod::StaticKey => {
            // Static key flow — same as before
            collect_static_api_key(&provider, &existing)?
        }
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

    tui::print_section("email configuration");

    let setup_email = tui::prompt_confirm(
        "Set up email integration to connect to your email service and prevent OpenClaw from seeing sensitive emails by filtering emails by sender?",
        existing.email_enabled.unwrap_or(false),
    )?;

    let email = if setup_email {
        let mode_options = if existing.email_mode == Some(OnboardEmailMode::Denylist) {
            vec!["Denylist", "Allowlist"]
        } else {
            vec!["Allowlist", "Denylist"]
        };
        let mode_choice = tui::prompt_select("Select email sender filter mode", mode_options)?;
        let mode = match mode_choice {
            "Denylist" => OnboardEmailMode::Denylist,
            _ => OnboardEmailMode::Allowlist,
        };

        let default_sender_rules_owned =
            if existing.email_mode == Some(mode) && !existing.email_sender_rules.is_empty() {
                Some(existing.email_sender_rules.join(", "))
            } else {
                None
            };
        let sender_rules_prompt = match mode {
            OnboardEmailMode::Allowlist => {
                "Enter allow_senders (comma-separated emails or @domain rules)"
            }
            OnboardEmailMode::Denylist => {
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
                } else {
                    for rule in rules {
                        if let Err(error) = crate::config::validate_sender_rule(&rule) {
                            return Ok(inquire::validator::Validation::Invalid(
                                format!("Invalid sender rule '{rule}': {error}").into(),
                            ));
                        }
                    }
                    Ok(inquire::validator::Validation::Valid)
                }
            },
        )?;
        let sender_rules = parse_sender_rules(&sender_rules_input);

        let default_email_virtual_key_owned = existing
            .email_account_virtual_key
            .clone()
            .unwrap_or_else(|| "{clawshell-virtual-key-email}".to_string());
        let account_virtual_key = tui::prompt_text(
            "Enter the virtual API key for email endpoint access",
            Some(&default_email_virtual_key_owned),
        )?;
        if account_virtual_key.trim().is_empty() {
            return Err("email virtual API key cannot be empty".into());
        }

        let preferred_imap_provider =
            infer_imap_provider_preset(existing.email_imap_host.as_deref());
        let provider_choice = tui::prompt_select(
            "Select email IMAP provider",
            email_imap_provider_options(preferred_imap_provider),
        )?;
        let provider = parse_imap_provider_choice(provider_choice);

        let mut imap_host;
        let mut imap_port;
        let is_custom_imap_provider;

        if let Some((default_host, default_port)) = email_provider_defaults(provider) {
            imap_host = default_host.to_string();
            imap_port = default_port;
            is_custom_imap_provider = false;
        } else {
            let default_custom_imap_host = existing.email_imap_host.as_deref().and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            });
            let custom_host = tui::prompt_text_validated(
                "Enter IMAP host",
                default_custom_imap_host,
                |input: &str| {
                    if input.trim().is_empty() {
                        return Ok(inquire::validator::Validation::Invalid(
                            "IMAP host cannot be empty".into(),
                        ));
                    }
                    Ok(inquire::validator::Validation::Valid)
                },
            )?;
            let default_custom_imap_port = existing
                .email_imap_port
                .unwrap_or(EMAIL_DEFAULT_IMAP_PORT)
                .to_string();
            let custom_port = tui::prompt_text_validated(
                "Enter IMAP port",
                Some(&default_custom_imap_port),
                |input: &str| match input.parse::<u16>() {
                    Ok(port) if port > 0 => Ok(inquire::validator::Validation::Valid),
                    _ => Ok(inquire::validator::Validation::Invalid(
                        "Please enter a valid port number (1-65535)".into(),
                    )),
                },
            )?;

            imap_host = custom_host.trim().to_string();
            imap_port = custom_port.parse::<u16>().unwrap();
            is_custom_imap_provider = true;
        }

        tui::print_info("IMAP server", &format!("{imap_host}:{imap_port}"));

        let default_email = existing.email_email.as_deref().unwrap_or("");
        let mut email = tui::prompt_text_validated(
            "Enter email address (e.g. hello@example.com)",
            if default_email.is_empty() {
                None
            } else {
                Some(default_email)
            },
            |input: &str| {
                let email = input.trim().to_ascii_lowercase();
                if email.is_empty() {
                    return Ok(inquire::validator::Validation::Invalid(
                        "email address cannot be empty".into(),
                    ));
                }
                if email.starts_with('@') || !email.contains('@') {
                    return Ok(inquire::validator::Validation::Invalid(
                        "Please enter a full email address".into(),
                    ));
                }
                if let Err(error) = crate::config::validate_sender_rule(&email) {
                    return Ok(inquire::validator::Validation::Invalid(
                        format!("Invalid email address: {error}").into(),
                    ));
                }
                Ok(inquire::validator::Validation::Valid)
            },
        )?;

        let mut app_password =
            if let Some(existing_password) = existing.email_app_password.as_deref() {
                tui::print_info(
                    "Existing email app password",
                    &mask_secret(existing_password),
                );
                if tui::prompt_confirm("Reuse existing email app password?", true)? {
                    existing_password.to_string()
                } else {
                    tui::prompt_password("Enter email app password (16-character app password)")?
                }
            } else {
                tui::prompt_password("Enter email app password (16-character app password)")?
            };
        if app_password.trim().is_empty() {
            return Err("email app password cannot be empty".into());
        }

        loop {
            match validate_email_imap_credentials(&email, &app_password, &imap_host, imap_port) {
                Ok(()) => {
                    tui::print_success("email IMAP login validated.");
                    break;
                }
                Err(error) => {
                    tui::print_warning(&format!("email IMAP login failed: {error}"));
                    let retry = tui::prompt_confirm(
                        "Update email credentials and retry IMAP validation?",
                        true,
                    )?;
                    if !retry {
                        return Err("email IMAP validation failed".into());
                    }

                    email = tui::prompt_text_validated(
                        "Enter email address (e.g. hello@example.com)",
                        Some(&email),
                        |input: &str| {
                            let email = input.trim().to_ascii_lowercase();
                            if email.is_empty() {
                                return Ok(inquire::validator::Validation::Invalid(
                                    "email address cannot be empty".into(),
                                ));
                            }
                            if email.starts_with('@') || !email.contains('@') {
                                return Ok(inquire::validator::Validation::Invalid(
                                    "Please enter a full email address".into(),
                                ));
                            }
                            if let Err(error) = crate::config::validate_sender_rule(&email) {
                                return Ok(inquire::validator::Validation::Invalid(
                                    format!("Invalid email address: {error}").into(),
                                ));
                            }
                            Ok(inquire::validator::Validation::Valid)
                        },
                    )?;
                    app_password = tui::prompt_password(
                        "Enter email app password (16-character app password)",
                    )?;
                    if app_password.trim().is_empty() {
                        return Err("email app password cannot be empty".into());
                    }

                    if is_custom_imap_provider {
                        imap_host = tui::prompt_text_validated(
                            "Enter IMAP host",
                            Some(&imap_host),
                            |input: &str| {
                                if input.trim().is_empty() {
                                    return Ok(inquire::validator::Validation::Invalid(
                                        "IMAP host cannot be empty".into(),
                                    ));
                                }
                                Ok(inquire::validator::Validation::Valid)
                            },
                        )?
                        .trim()
                        .to_string();

                        let imap_port_default = imap_port.to_string();
                        let imap_port_value = tui::prompt_text_validated(
                            "Enter IMAP port",
                            Some(&imap_port_default),
                            |input: &str| match input.parse::<u16>() {
                                Ok(port) if port > 0 => Ok(inquire::validator::Validation::Valid),
                                _ => Ok(inquire::validator::Validation::Invalid(
                                    "Please enter a valid port number (1-65535)".into(),
                                )),
                            },
                        )?;
                        imap_port = imap_port_value.parse::<u16>().unwrap();
                    }
                }
            }
        }

        Some(OnboardEmailConfig {
            mode,
            sender_rules,
            account_virtual_key,
            email: email.trim().to_string(),
            app_password: app_password.trim().to_string(),
            imap_host,
            imap_port,
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
        "Enter the OpenClaw configuration file path (for backup/recovery)",
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
        auth_method,
        real_api_key,
        virtual_api_key,
        openclaw_config_path: PathBuf::from(openclaw_config_path),
        server_host,
        server_port,
        email,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::test_support::vfs_write;
    use vfs::{MemoryFS, VfsPath};

    #[test]
    fn test_sender_rule_validation_matches_runtime_rules() {
        assert!(crate::config::validate_sender_rule("alice@example.com").is_ok());
        assert!(crate::config::validate_sender_rule("@trusted.org").is_ok());
        assert!(crate::config::validate_sender_rule("@.example.com").is_err());
        assert!(crate::config::validate_sender_rule("alice@example..com").is_err());
        assert!(crate::config::validate_sender_rule("aliceexample.com").is_err());
        assert!(crate::config::validate_sender_rule("@").is_err());
        assert!(crate::config::validate_sender_rule("alice@localhost").is_err());
    }

    #[test]
    fn test_infer_imap_provider_preset_from_host() {
        assert_eq!(
            infer_imap_provider_preset(None),
            EmailImapProviderPreset::Gmail
        );
        assert_eq!(
            infer_imap_provider_preset(Some("imap.gmail.com")),
            EmailImapProviderPreset::Gmail
        );
        assert_eq!(
            infer_imap_provider_preset(Some("imap-mail.outlook.com")),
            EmailImapProviderPreset::Outlook
        );
        assert_eq!(
            infer_imap_provider_preset(Some("imap.custom.local")),
            EmailImapProviderPreset::Other
        );
    }

    #[test]
    fn test_email_provider_defaults_for_presets() {
        assert_eq!(
            email_provider_defaults(EmailImapProviderPreset::Gmail),
            Some(("imap.gmail.com", 993))
        );
        assert_eq!(
            email_provider_defaults(EmailImapProviderPreset::Outlook),
            Some(("imap-mail.outlook.com", 993))
        );
        assert_eq!(
            email_provider_defaults(EmailImapProviderPreset::Other),
            None
        );
    }

    #[test]
    fn test_parse_imap_provider_choice() {
        assert_eq!(
            parse_imap_provider_choice(EMAIL_PROVIDER_PRESET_GMAIL),
            EmailImapProviderPreset::Gmail
        );
        assert_eq!(
            parse_imap_provider_choice(EMAIL_PROVIDER_PRESET_OUTLOOK),
            EmailImapProviderPreset::Outlook
        );
        assert_eq!(
            parse_imap_provider_choice(EMAIL_PROVIDER_PRESET_OTHER),
            EmailImapProviderPreset::Other
        );
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

        let with_imap_host = ExistingConfig {
            email_imap_host: Some("imap.custom.local".to_string()),
            ..Default::default()
        };
        assert!(with_imap_host.has_any());

        let with_imap_port = ExistingConfig {
            email_imap_port: Some(143),
            ..Default::default()
        };
        assert!(with_imap_port.has_any());
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
    fn test_load_existing_config_reads_email_defaults() {
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

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com", "@trusted.org"]

[[email.accounts]]
virtual_key = "{clawshell-virtual-key-openai}"
email = "bot@gmail.com"
app_password = "existing-app-password"
imap_host = "imap.gmail.com"
imap_port = 993
"#,
        );

        let config_dir = root.join("etc/clawshell").unwrap();
        let existing = load_existing_config_from_vfs(&config_dir).unwrap();
        assert_eq!(existing.email_enabled, Some(true));
        assert_eq!(existing.email_mode, Some(OnboardEmailMode::Allowlist));
        assert_eq!(
            existing.email_sender_rules,
            vec!["alice@example.com".to_string(), "@trusted.org".to_string()]
        );
        assert_eq!(
            existing.email_account_virtual_key.as_deref(),
            Some("{clawshell-virtual-key-openai}")
        );
        assert_eq!(existing.email_email.as_deref(), Some("bot@gmail.com"));
        assert_eq!(
            existing.email_app_password.as_deref(),
            Some("existing-app-password")
        );
        assert_eq!(existing.email_imap_host.as_deref(), Some("imap.gmail.com"));
        assert_eq!(existing.email_imap_port, Some(993));
    }

    #[test]
    fn test_load_existing_config_prefers_matching_email_account() {
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

[email]
enabled = true
mode = "denylist"
deny_senders = ["@blocked.local"]

[[email.accounts]]
virtual_key = "vk-other"
email = "other@email.com"
app_password = "other-app-password"
imap_host = "imap.gmail.com"
imap_port = 993

[[email.accounts]]
virtual_key = "vk-match"
email = "match@email.com"
app_password = "match-app-password"
imap_host = "imap.gmail.com"
imap_port = 993
"#,
        );

        let config_dir = root.join("etc/clawshell").unwrap();
        let existing = load_existing_config_from_vfs(&config_dir).unwrap();
        assert_eq!(existing.email_mode, Some(OnboardEmailMode::Denylist));
        assert_eq!(
            existing.email_sender_rules,
            vec!["@blocked.local".to_string()]
        );
        assert_eq!(
            existing.email_account_virtual_key.as_deref(),
            Some("vk-match")
        );
        assert_eq!(existing.email_email.as_deref(), Some("match@email.com"));
        assert_eq!(
            existing.email_app_password.as_deref(),
            Some("match-app-password")
        );
        assert_eq!(existing.email_imap_host.as_deref(), Some("imap.gmail.com"));
        assert_eq!(existing.email_imap_port, Some(993));
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
}
