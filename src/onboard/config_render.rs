use super::types::{OnboardAuthMethod, OnboardConfig, OnboardEmailMode};

/// Return the default OpenClaw config path.
pub fn default_openclaw_config_path() -> String {
    if let Ok(home) = std::env::var("HOME") {
        format!("{}/.openclaw/openclaw.json", home)
    } else {
        "~/.openclaw/openclaw.json".to_string()
    }
}

/// Generate the ClawShell TOML configuration content with the given key mapping.
pub fn generate_clawshell_config(config: &OnboardConfig) -> String {
    let key_section = match &config.auth_method {
        OnboardAuthMethod::OAuth { provider_id } => {
            format!(
                r#"[[keys]]
virtual_key = {virtual_key}
provider = {provider}
auth = "oauth"
oauth_provider = {oauth_provider}
"#,
                virtual_key = toml_string(&config.virtual_api_key),
                provider = toml_string(&config.provider),
                oauth_provider = toml_string(provider_id),
            )
        }
        OnboardAuthMethod::StaticKey => {
            format!(
                r#"[[keys]]
virtual_key = {virtual_key}
real_key = {real_key}
provider = {provider}
"#,
                virtual_key = toml_string(&config.virtual_api_key),
                real_key = toml_string(&config.real_api_key),
                provider = toml_string(&config.provider),
            )
        }
    };

    let oauth_providers_section = match &config.auth_method {
        OnboardAuthMethod::OAuth { provider_id } => {
            format!(
                r#"
[[oauth_providers]]
provider = {provider_id}
"#,
                provider_id = toml_string(provider_id),
            )
        }
        OnboardAuthMethod::StaticKey => String::new(),
    };

    let mut output = format!(
        r#"# ClawShell Configuration
version = "{version}"
log_level = "info"

[server]
host = "{host}"
port = {port}

[upstream]
openai_base_url = "https://api.openai.com"
openrouter_base_url = "https://openrouter.ai/api"
anthropic_base_url = "https://api.anthropic.com"
minimax_base_url = "https://api.minimax.io"

{key_section}[dlp]
scan_responses = true
patterns = [
    {{ name = "ssn",             regex = '\b\d{{3}}-\d{{2}}-\d{{4}}\b',                                            action = "redact" }},
    {{ name = "visa_card",       regex = '\b4[0-9]{{12}}(?:[0-9]{{3}})?\b',                                        action = "redact" }},
    {{ name = "visa_mastercard", regex = '\b(?:4[0-9]{{12}}(?:[0-9]{{3}})?|5[1-5][0-9]{{14}})\b',                  action = "redact" }},
    {{ name = "mastercard",      regex = '\b5[1-5][0-9]{{14}}\b',                                                  action = "redact" }},
    {{ name = "amex_card",       regex = '\b3[47][0-9]{{13}}\b',                                                   action = "redact" }},
]
{oauth_providers_section}"#,
        version = env!("CARGO_PKG_VERSION"),
        host = config.server_host,
        port = config.server_port,
        key_section = key_section,
        oauth_providers_section = oauth_providers_section,
    );

    if let Some(email) = &config.email {
        let (allow_senders, deny_senders) = match email.mode {
            OnboardEmailMode::Allowlist => (toml_string_array(&email.sender_rules), "[]".into()),
            OnboardEmailMode::Denylist => ("[]".into(), toml_string_array(&email.sender_rules)),
        };

        output.push_str(&format!(
            r#"
[email]
enabled = true
mode = "{mode}"
allow_senders = {allow_senders}
deny_senders = {deny_senders}
default_max_results = 50

[[email.accounts]]
virtual_key = {virtual_key}
email = {email}
app_password = {app_password}
imap_host = {imap_host}
imap_port = {imap_port}
"#,
            mode = email.mode.as_toml_value(),
            allow_senders = allow_senders,
            deny_senders = deny_senders,
            virtual_key = toml_string(&email.account_virtual_key),
            email = toml_string(&email.email),
            app_password = toml_string(&email.app_password),
            imap_host = toml_string(&email.imap_host),
            imap_port = email.imap_port,
        ));
    }

    output
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::test_support::test_config;
    use crate::onboard::types::{OnboardEmailConfig, OnboardEmailMode};

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
        assert!(!toml_str.contains("[email]"));
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
    fn test_generate_clawshell_config_with_email_imap_credentials() {
        let mut config = test_config();
        config.email = Some(OnboardEmailConfig {
            mode: OnboardEmailMode::Denylist,
            sender_rules: vec!["@blocked.local".to_string()],
            account_virtual_key: "{email-virtual-key}".to_string(),
            email: "bot@gmail.com".to_string(),
            app_password: "abcd efgh ijkl mnop".to_string(),
            imap_host: "imap.gmail.com".to_string(),
            imap_port: 993,
        });

        let toml_str = generate_clawshell_config(&config);
        assert!(toml_str.contains("[email]"));
        assert!(toml_str.contains("enabled = true"));
        assert!(toml_str.contains("mode = \"denylist\""));
        assert!(toml_str.contains("allow_senders = []"));
        assert!(toml_str.contains("deny_senders = [\"@blocked.local\"]"));
        assert!(toml_str.contains("email = \"bot@gmail.com\""));
        assert!(toml_str.contains("app_password = \"abcd efgh ijkl mnop\""));
        assert!(toml_str.contains("imap_host = \"imap.gmail.com\""));
        assert!(toml_str.contains("imap_port = 993"));
        assert!(!toml_str.contains("refresh_token ="));
    }

    #[test]
    fn test_generate_clawshell_config_with_outlook_imap_credentials() {
        let mut config = test_config();
        config.email = Some(OnboardEmailConfig {
            mode: OnboardEmailMode::Allowlist,
            sender_rules: vec!["@trusted.local".to_string()],
            account_virtual_key: "vk-email-001".to_string(),
            email: "bot@outlook.com".to_string(),
            app_password: "abcd efgh ijkl mnop".to_string(),
            imap_host: "imap-mail.outlook.com".to_string(),
            imap_port: 993,
        });

        let toml_str = generate_clawshell_config(&config);
        assert!(toml_str.contains("imap_host = \"imap-mail.outlook.com\""));
        assert!(toml_str.contains("imap_port = 993"));
    }

    #[test]
    fn test_default_openclaw_config_path() {
        let path = default_openclaw_config_path();
        assert!(path.contains(".openclaw/openclaw.json"));
    }
}
