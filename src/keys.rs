use crate::config::Provider;

use std::collections::BTreeMap;
use tracing::{debug, trace};

#[derive(Debug, Clone)]
pub enum KeySource {
    Static { real_key: String },
    OAuth { provider_id: String },
}

#[derive(Debug, Clone)]
pub struct ResolvedKey {
    pub source: KeySource,
    pub provider: Provider,
}

#[derive(Debug, Clone)]
pub struct KeyManager {
    mappings: BTreeMap<String, ResolvedKey>,
}

impl KeyManager {
    pub fn new(mappings: BTreeMap<String, ResolvedKey>) -> Self {
        debug!(key_count = mappings.len(), "Key manager initialized");
        Self { mappings }
    }

    /// Extracts the virtual key from the Authorization header value.
    /// Expects "Bearer <virtual_key>" format.
    pub fn extract_virtual_key(auth_header: &str) -> Option<&str> {
        let trimmed = auth_header.trim();
        if let Some(key) = trimmed.strip_prefix("Bearer ") {
            let key = key.trim();
            if key.is_empty() {
                trace!("Empty Bearer token");
                None
            } else {
                trace!(virtual_key = %key, "Extracted virtual key from Bearer token");
                Some(key)
            }
        } else {
            trace!("Authorization header missing Bearer prefix");
            None
        }
    }

    /// Looks up the real API key and provider for a given virtual key.
    pub fn resolve(&self, virtual_key: &str) -> Option<&ResolvedKey> {
        let result = self.mappings.get(virtual_key);
        match &result {
            Some(resolved) => {
                debug!(virtual_key = %virtual_key, provider = ?resolved.provider, "Virtual key resolved");
            }
            None => {
                debug!(virtual_key = %virtual_key, "Virtual key not found");
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_static_map(entries: Vec<(&str, &str, Provider)>) -> BTreeMap<String, ResolvedKey> {
        entries
            .into_iter()
            .map(|(vk, rk, p)| {
                (
                    vk.to_string(),
                    ResolvedKey {
                        source: KeySource::Static {
                            real_key: rk.to_string(),
                        },
                        provider: p,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn test_extract_virtual_key_bearer() {
        assert_eq!(
            KeyManager::extract_virtual_key("Bearer vk-test-123"),
            Some("vk-test-123")
        );
    }

    #[test]
    fn test_extract_virtual_key_no_bearer() {
        assert_eq!(KeyManager::extract_virtual_key("vk-test-123"), None);
    }

    #[test]
    fn test_extract_virtual_key_empty() {
        assert_eq!(KeyManager::extract_virtual_key("Bearer "), None);
    }

    #[test]
    fn test_resolve_existing_key() {
        let map = make_static_map(vec![("vk-1", "sk-real-1", Provider::Openai)]);
        let km = KeyManager::new(map);
        let resolved = km.resolve("vk-1").unwrap();
        match &resolved.source {
            KeySource::Static { real_key } => assert_eq!(real_key, "sk-real-1"),
            KeySource::OAuth { .. } => panic!("expected Static"),
        }
        assert_eq!(resolved.provider, Provider::Openai);
    }

    #[test]
    fn test_resolve_missing_key() {
        let km = KeyManager::new(BTreeMap::new());
        assert!(km.resolve("vk-nonexistent").is_none());
    }

    #[test]
    fn test_multiple_virtual_to_same_real() {
        let map = make_static_map(vec![
            ("vk-1", "sk-shared", Provider::Openai),
            ("vk-2", "sk-shared", Provider::Openai),
        ]);
        let km = KeyManager::new(map);
        match &km.resolve("vk-1").unwrap().source {
            KeySource::Static { real_key } => assert_eq!(real_key, "sk-shared"),
            _ => panic!("expected Static"),
        }
        match &km.resolve("vk-2").unwrap().source {
            KeySource::Static { real_key } => assert_eq!(real_key, "sk-shared"),
            _ => panic!("expected Static"),
        }
    }

    #[test]
    fn test_resolve_anthropic_provider() {
        let map = make_static_map(vec![("vk-ant", "sk-ant-key", Provider::Anthropic)]);
        let km = KeyManager::new(map);
        let resolved = km.resolve("vk-ant").unwrap();
        match &resolved.source {
            KeySource::Static { real_key } => assert_eq!(real_key, "sk-ant-key"),
            _ => panic!("expected Static"),
        }
        assert_eq!(resolved.provider, Provider::Anthropic);
    }

    #[test]
    fn test_resolve_oauth_key() {
        let mut map = BTreeMap::new();
        map.insert(
            "vk-oauth".to_string(),
            ResolvedKey {
                source: KeySource::OAuth {
                    provider_id: "codex".to_string(),
                },
                provider: Provider::Openai,
            },
        );
        let km = KeyManager::new(map);
        let resolved = km.resolve("vk-oauth").unwrap();
        match &resolved.source {
            KeySource::OAuth { provider_id } => assert_eq!(provider_id, "codex"),
            _ => panic!("expected OAuth"),
        }
        assert_eq!(resolved.provider, Provider::Openai);
    }
}
