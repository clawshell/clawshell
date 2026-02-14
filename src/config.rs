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
