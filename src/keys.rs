use crate::config::Provider;

use std::collections::BTreeMap;
use tracing::{debug, trace};

#[derive(Debug, Clone)]
pub struct ResolvedKey {
    pub real_key: String,
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

    fn make_map(entries: Vec<(&str, &str, Provider)>) -> BTreeMap<String, ResolvedKey> {
        entries
            .into_iter()
            .map(|(vk, rk, p)| {
                (
                    vk.to_string(),
                    ResolvedKey {
                        real_key: rk.to_string(),
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
        let map = make_map(vec![("vk-1", "sk-real-1", Provider::Openai)]);
        let km = KeyManager::new(map);
        let resolved = km.resolve("vk-1").unwrap();
        assert_eq!(resolved.real_key, "sk-real-1");
        assert_eq!(resolved.provider, Provider::Openai);
    }

    #[test]
    fn test_resolve_missing_key() {
        let km = KeyManager::new(BTreeMap::new());
        assert!(km.resolve("vk-nonexistent").is_none());
    }

    #[test]
    fn test_multiple_virtual_to_same_real() {
        let map = make_map(vec![
            ("vk-1", "sk-shared", Provider::Openai),
            ("vk-2", "sk-shared", Provider::Openai),
        ]);
        let km = KeyManager::new(map);
        assert_eq!(km.resolve("vk-1").unwrap().real_key, "sk-shared");
        assert_eq!(km.resolve("vk-2").unwrap().real_key, "sk-shared");
    }

    #[test]
    fn test_resolve_anthropic_provider() {
        let map = make_map(vec![("vk-ant", "sk-ant-key", Provider::Anthropic)]);
        let km = KeyManager::new(map);
        let resolved = km.resolve("vk-ant").unwrap();
        assert_eq!(resolved.real_key, "sk-ant-key");
        assert_eq!(resolved.provider, Provider::Anthropic);
    }
}
