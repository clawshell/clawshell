use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Openai,
    Anthropic,
}

impl Provider {
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Provider::Openai => "https://api.openai.com",
            Provider::Anthropic => "https://api.anthropic.com",
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub upstream: UpstreamConfig,
    #[serde(default)]
    pub keys: Vec<KeyMapping>,
    #[serde(default)]
    pub dlp: DlpConfig,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
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
pub struct UpstreamConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    #[serde(default = "default_anthropic_version")]
    pub anthropic_version: String,
}

fn default_anthropic_version() -> String {
    "2023-06-01".to_string()
}

fn default_base_url() -> String {
    "https://api.openai.com".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
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
pub struct DlpPattern {
    pub name: String,
    pub regex: String,
    #[serde(default)]
    pub action: DlpAction,
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    #[cfg(test)]
    pub fn parse(content: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let config: Config = toml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        for pattern in &self.dlp.patterns {
            Regex::new(&pattern.regex)
                .map_err(|e| format!("Invalid DLP regex for '{}': {}", pattern.name, e))?;
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
            Provider::Openai => self.upstream.base_url.clone(),
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
}
