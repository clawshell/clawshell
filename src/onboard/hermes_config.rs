//! Pure command builders for `hermes config set ...` invocations used when
//! onboarding wires Hermes Agent through ClawShell.
//!
//! Hermes Agent stores config in `~/.hermes/config.yaml` with API keys in
//! `~/.hermes/.env`; its `hermes config set <key> <value>` CLI auto-routes
//! secrets to `.env` based on a hardcoded key-name list. `model.api_key`
//! isn't in that list, so it lands in `config.yaml` under `model:` — which
//! is fine here because a ClawShell virtual key is only meaningful against
//! `127.0.0.1:18790` and the real upstream credentials never leave
//! `/etc/clawshell/clawshell.toml`.
//!
//! Hermes has no `config unset` subcommand, so revert is done by setting
//! `model.provider` back to `auto` (its documented auto-detect default).

use super::types::OnboardConfig;

/// Build the list of `hermes config set <key> <value>` argv lists to run in
/// order to point Hermes at ClawShell.
///
/// The virtual API key (not the real upstream key) must be what ends up in
/// Hermes config — ClawShell will translate it at the proxy boundary.
pub fn hermes_config_set_commands(config: &OnboardConfig) -> Vec<Vec<String>> {
    let base_url = format!("http://{}:{}/v1", config.server_host, config.server_port);
    vec![
        hermes_set(&["model.provider", "clawshell"]),
        hermes_set(&["model.base_url", &base_url]),
        hermes_set(&["model.default", &config.model]),
        hermes_set(&["model.api_key", &config.virtual_api_key]),
    ]
}

fn hermes_set(key_value: &[&str; 2]) -> Vec<String> {
    vec![
        "config".to_string(),
        "set".to_string(),
        key_value[0].to_string(),
        key_value[1].to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::test_support::test_config;

    #[test]
    fn builds_four_commands_in_documented_order() {
        let config = test_config();
        let commands = hermes_config_set_commands(&config);

        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[0],
            vec!["config", "set", "model.provider", "clawshell"]
        );
        assert_eq!(
            commands[1],
            vec![
                "config",
                "set",
                "model.base_url",
                "http://127.0.0.1:18790/v1"
            ]
        );
        assert_eq!(
            commands[2],
            vec!["config", "set", "model.default", "gpt-5.2"]
        );
        assert_eq!(
            commands[3],
            vec![
                "config",
                "set",
                "model.api_key",
                "{clawshell-virtual-key-openai}",
            ]
        );
    }

    #[test]
    fn uses_virtual_key_not_real_key() {
        let config = test_config();
        let commands = hermes_config_set_commands(&config);
        let api_key_cmd = commands
            .iter()
            .find(|cmd| cmd.get(2).map(|k| k == "model.api_key").unwrap_or(false))
            .expect("model.api_key command present");
        assert_eq!(api_key_cmd[3], config.virtual_api_key);
        assert_ne!(api_key_cmd[3], config.real_api_key);
    }

    #[test]
    fn base_url_reflects_custom_host_port() {
        let mut config = test_config();
        config.server_host = "10.0.0.5".to_string();
        config.server_port = 9000;
        let commands = hermes_config_set_commands(&config);
        assert_eq!(commands[1][3], "http://10.0.0.5:9000/v1");
    }

    #[test]
    fn model_name_propagates_from_onboard_config() {
        let mut config = test_config();
        config.model = "claude-sonnet-4-5-20250929".to_string();
        let commands = hermes_config_set_commands(&config);
        assert_eq!(commands[2][3], "claude-sonnet-4-5-20250929");
    }
}
