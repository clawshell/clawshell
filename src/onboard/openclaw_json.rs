use super::types::OnboardConfig;
use serde_json::Value;

const OPENCLAW_LEGACY_ENV_KEYS: [&str; 3] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
];

fn is_legacy_env_key(key: &str) -> bool {
    OPENCLAW_LEGACY_ENV_KEYS.contains(&key)
}

fn remove_matching_legacy_env_keys(
    env: &mut serde_json::Map<String, Value>,
    mapped_real_key: &str,
) {
    let keys_to_remove: Vec<String> = env
        .iter()
        .filter_map(|(key, value)| {
            if !is_legacy_env_key(key) {
                return None;
            }
            value
                .as_str()
                .is_some_and(|v| v.trim() == mapped_real_key)
                .then(|| key.clone())
        })
        .collect();

    for key in keys_to_remove {
        env.remove(&key);
    }
}

/// Modify the OpenClaw configuration JSON to add ClawShell entries.
///
/// This function:
/// 1. Removes legacy OpenAI/Anthropic credential keys from `env` when their
///    value matches the real key currently mapped by the configured virtual key
/// 2. Sets `"CLAWSHELL_API_KEY"` in the `env` object
/// 3. Appends a model entry to `agents.defaults.models`
/// 4. Appends a provider entry to `models.providers`
pub fn patch_openclaw_config_for_clawshell(
    content: &str,
    config: &OnboardConfig,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut json: Value = serde_json::from_str(content)?;

    // 1. Remove mapped legacy provider key(s), then set CLAWSHELL_API_KEY in env
    ensure_nested_object(&mut json, &["env"]);
    if let Some(env) = json.get_mut("env").and_then(Value::as_object_mut) {
        remove_matching_legacy_env_keys(env, &config.real_api_key);
    }
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
#[cfg(test)]
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
pub fn remove_clawshell_openclaw_entries(
    content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
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
    use crate::onboard::test_support::test_config;

    #[test]
    fn test_modify_openclaw_config_empty_json() {
        let config = test_config();
        let result = patch_openclaw_config_for_clawshell("{}", &config).unwrap();
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
        let result = patch_openclaw_config_for_clawshell(existing, &config).unwrap();
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
    fn test_modify_openclaw_config_removes_only_mapped_legacy_env_key() {
        let existing = r#"{
            "env": {
                "OPENAI_API_KEY": "sk-real-key-123",
                "ANTHROPIC_API_KEY": "sk-ant-old",
                "ANTHROPIC_OAUTH_TOKEN": "oauth-old",
                "EXISTING_VAR": "value"
            }
        }"#;

        let config = test_config();
        let result = patch_openclaw_config_for_clawshell(existing, &config).unwrap();
        let json: Value = serde_json::from_str(&result).unwrap();
        let env = json["env"].as_object().unwrap();

        assert_eq!(env["EXISTING_VAR"], "value");
        assert_eq!(env["CLAWSHELL_API_KEY"], "{clawshell-virtual-key-openai}");
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert_eq!(env["ANTHROPIC_API_KEY"], "sk-ant-old");
        assert_eq!(env["ANTHROPIC_OAUTH_TOKEN"], "oauth-old");
    }

    #[test]
    fn test_modify_openclaw_config_anthropic() {
        let mut config = test_config();
        config.provider = "anthropic".to_string();
        config.model = "claude-sonnet-4-5-20250929".to_string();

        let result = patch_openclaw_config_for_clawshell("{}", &config).unwrap();
        let json: Value = serde_json::from_str(&result).unwrap();

        let prov = &json["models"]["providers"]["clawshell"];
        assert_eq!(prov["api"], "openai-completions");
        assert_eq!(prov["models"][0]["id"], "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn test_modify_openclaw_config_invalid_json() {
        let config = test_config();
        let result = patch_openclaw_config_for_clawshell("not json", &config);
        assert!(result.is_err());
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

        let result = remove_clawshell_openclaw_entries(content).unwrap();
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

        let result = remove_clawshell_openclaw_entries(content).unwrap();
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
