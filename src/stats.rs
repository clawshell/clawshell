use crate::email::{extract_sender_address, normalize_sender_rule};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::{debug, warn};

/// Hard cap on the number of distinct sender addresses the stats map will
/// track. Further unique addresses are aggregated under [`OVERFLOW_KEY`]
/// so the map size is strictly bounded.
pub const MAX_TRACKED_ADDRESSES: usize = 10_000;

/// Synthetic key used when the address map is full. Its count equals the
/// number of filtered messages from addresses that could not be tracked
/// individually.
pub const OVERFLOW_KEY: &str = "<overflow>";

/// Max byte length of a single address we are willing to store. RFC 5321
/// puts the hard limit for a path at 256 octets; 320 leaves slack for
/// display-name residue before we reject the entry.
const MAX_ADDRESS_LEN: usize = 320;

pub struct Stats {
    requests_total: AtomicU64,
    prompt_tokens_total: AtomicU64,
    completion_tokens_total: AtomicU64,
    total_tokens_total: AtomicU64,
    emails_filtered_total: AtomicU64,
    /// Key: filtered sender address
    /// Value: count of messages from that sender that were filtered
    filtered_email_addresses: Mutex<BTreeMap<String, u64>>,
    persist_path: Option<PathBuf>,
    dirty: AtomicBool,
    overflow_warned_this_cycle: AtomicBool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct StatsSnapshot {
    pub requests_total: u64,
    pub prompt_tokens_total: u64,
    pub completion_tokens_total: u64,
    pub total_tokens_total: u64,
    pub emails_filtered_total: u64,
    /// Key: filtered sender address
    /// Value: count of messages from that sender that were filtered
    pub filtered_email_addresses: BTreeMap<String, u64>,
}

impl Stats {
    pub fn new(persist_path: Option<PathBuf>) -> Self {
        let snapshot = match persist_path.as_deref() {
            Some(path) if path.exists() => match Self::load_snapshot(path) {
                Ok(snap) => snap,
                Err(err) => {
                    warn!(
                        path = %path.display(),
                        error = %err,
                        "Failed to load stats from disk — starting with empty counters"
                    );
                    StatsSnapshot::default()
                }
            },
            _ => StatsSnapshot::default(),
        };

        Self {
            requests_total: AtomicU64::new(snapshot.requests_total),
            prompt_tokens_total: AtomicU64::new(snapshot.prompt_tokens_total),
            completion_tokens_total: AtomicU64::new(snapshot.completion_tokens_total),
            total_tokens_total: AtomicU64::new(snapshot.total_tokens_total),
            emails_filtered_total: AtomicU64::new(snapshot.emails_filtered_total),
            filtered_email_addresses: Mutex::new(snapshot.filtered_email_addresses),
            persist_path,
            dirty: AtomicBool::new(false),
            overflow_warned_this_cycle: AtomicBool::new(false),
        }
    }

    fn load_snapshot(path: &Path) -> std::io::Result<StatsSnapshot> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::other(format!("failed to parse stats file: {e}")))
    }

    pub fn record_request(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Parse a response body for an LLM `usage` object and add it to the
    /// running totals. Accepts both OpenAI-shaped
    /// (`{prompt_tokens, completion_tokens, total_tokens}`) and
    /// Anthropic-shaped (`{input_tokens, output_tokens}`) usage blocks.
    /// Silently no-ops for non-JSON bodies or bodies without a usage object.
    pub fn record_tokens_from_usage(&self, body: &[u8]) {
        if body.is_empty() {
            return;
        }
        let Ok(json) = serde_json::from_slice::<Value>(body) else {
            return;
        };
        let Some(usage) = json.get("usage") else {
            return;
        };

        let prompt = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| prompt.saturating_add(completion));

        if prompt == 0 && completion == 0 && total == 0 {
            return;
        }

        self.prompt_tokens_total
            .fetch_add(prompt, Ordering::Relaxed);
        self.completion_tokens_total
            .fetch_add(completion, Ordering::Relaxed);
        self.total_tokens_total.fetch_add(total, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Record that a message from `from_header` was hidden by the email
    /// policy. `from_header` is the raw `From:` value; we extract the bare
    /// address when possible and fall back to a normalized form otherwise.
    pub fn record_email_filtered(&self, from_header: &str) {
        self.emails_filtered_total.fetch_add(1, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);

        let key = extract_sender_address(from_header)
            .unwrap_or_else(|| normalize_sender_rule(from_header));
        if key.is_empty() || key.len() > MAX_ADDRESS_LEN {
            // Over-long or empty keys are aggregated into the overflow bucket
            // so we still account for the filter event without letting a
            // malformed From header explode the map.
            self.bump_overflow();
            return;
        }

        let mut map = match self.filtered_email_addresses.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(count) = map.get_mut(&key) {
            *count = count.saturating_add(1);
            return;
        }
        if map.len() < MAX_TRACKED_ADDRESSES {
            map.insert(key, 1);
            return;
        }
        // Cap reached: funnel into the overflow bucket without growing the map.
        let overflow = map.entry(OVERFLOW_KEY.to_string()).or_insert(0);
        *overflow = overflow.saturating_add(1);
        drop(map);
        if !self
            .overflow_warned_this_cycle
            .swap(true, Ordering::Relaxed)
        {
            warn!(
                cap = MAX_TRACKED_ADDRESSES,
                "filtered_email_addresses map hit its cap; additional unique senders are being aggregated under '{OVERFLOW_KEY}'"
            );
        }
    }

    fn bump_overflow(&self) {
        let mut map = match self.filtered_email_addresses.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let overflow = map.entry(OVERFLOW_KEY.to_string()).or_insert(0);
        *overflow = overflow.saturating_add(1);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let map = match self.filtered_email_addresses.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        StatsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            prompt_tokens_total: self.prompt_tokens_total.load(Ordering::Relaxed),
            completion_tokens_total: self.completion_tokens_total.load(Ordering::Relaxed),
            total_tokens_total: self.total_tokens_total.load(Ordering::Relaxed),
            emails_filtered_total: self.emails_filtered_total.load(Ordering::Relaxed),
            filtered_email_addresses: map,
        }
    }

    /// Write the current snapshot to disk if configured and there are
    /// unsaved changes. Atomic: write to a sibling temp file, then rename.
    pub fn persist(&self) -> std::io::Result<()> {
        let Some(path) = self.persist_path.as_ref() else {
            return Ok(());
        };
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return Ok(());
        }
        // Reset the overflow-warned flag so the next cycle can log again if
        // overflow is still happening. We do this regardless of whether the
        // write succeeds; worst case is an extra log line per cycle.
        self.overflow_warned_this_cycle
            .store(false, Ordering::Relaxed);

        let snapshot = self.snapshot();
        let content = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| std::io::Error::other(format!("failed to serialize stats: {e}")))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, content)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
        }

        std::fs::rename(&tmp_path, path)?;
        debug!(path = %path.display(), "stats persisted to disk");
        Ok(())
    }
}

impl std::fmt::Debug for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stats")
            .field("persist_path", &self.persist_path)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_snapshots_requests() {
        let stats = Stats::new(None);
        stats.record_request();
        stats.record_request();
        stats.record_request();
        let snap = stats.snapshot();
        assert_eq!(snap.requests_total, 3);
    }

    #[test]
    fn parses_openai_shaped_usage() {
        let stats = Stats::new(None);
        let body = br#"{
            "id": "chatcmpl-1",
            "choices": [],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
        }"#;
        stats.record_tokens_from_usage(body);
        let snap = stats.snapshot();
        assert_eq!(snap.prompt_tokens_total, 10);
        assert_eq!(snap.completion_tokens_total, 3);
        assert_eq!(snap.total_tokens_total, 13);
    }

    #[test]
    fn parses_anthropic_shaped_usage() {
        let stats = Stats::new(None);
        let body = br#"{"usage": {"input_tokens": 7, "output_tokens": 5}}"#;
        stats.record_tokens_from_usage(body);
        let snap = stats.snapshot();
        assert_eq!(snap.prompt_tokens_total, 7);
        assert_eq!(snap.completion_tokens_total, 5);
        assert_eq!(snap.total_tokens_total, 12);
    }

    #[test]
    fn token_parse_is_no_op_for_garbage_or_missing_usage() {
        let stats = Stats::new(None);
        stats.record_tokens_from_usage(b"not json at all");
        stats.record_tokens_from_usage(b"{\"id\": \"x\"}");
        stats.record_tokens_from_usage(b"");
        let snap = stats.snapshot();
        assert_eq!(snap.prompt_tokens_total, 0);
        assert_eq!(snap.completion_tokens_total, 0);
        assert_eq!(snap.total_tokens_total, 0);
    }

    #[test]
    fn filtered_address_dedupes_and_counts() {
        let stats = Stats::new(None);
        stats.record_email_filtered("Spammer <Spam@Example.COM>");
        stats.record_email_filtered("spam@example.com");
        stats.record_email_filtered("\"Bob\" <bob@example.com>");
        let snap = stats.snapshot();
        assert_eq!(snap.emails_filtered_total, 3);
        assert_eq!(
            snap.filtered_email_addresses.get("spam@example.com"),
            Some(&2)
        );
        assert_eq!(
            snap.filtered_email_addresses.get("bob@example.com"),
            Some(&1)
        );
    }

    #[test]
    fn filtered_address_overflow_bucket() {
        let stats = Stats::new(None);
        // Fill the map to exactly MAX_TRACKED_ADDRESSES unique addresses.
        for i in 0..MAX_TRACKED_ADDRESSES {
            stats.record_email_filtered(&format!("user{i}@example.com"));
        }
        // The next N unique addresses should all land in the overflow bucket.
        for i in 0..5 {
            stats.record_email_filtered(&format!("overflow{i}@example.com"));
        }
        // An already-tracked address should still increment its own counter.
        stats.record_email_filtered("user0@example.com");

        let snap = stats.snapshot();
        assert_eq!(
            snap.emails_filtered_total,
            (MAX_TRACKED_ADDRESSES as u64) + 5 + 1
        );
        // Map is capped at MAX + 1 (the overflow sentinel).
        assert_eq!(
            snap.filtered_email_addresses.len(),
            MAX_TRACKED_ADDRESSES + 1
        );
        assert_eq!(snap.filtered_email_addresses.get(OVERFLOW_KEY), Some(&5));
        assert_eq!(
            snap.filtered_email_addresses.get("user0@example.com"),
            Some(&2)
        );
        // Totals add up: sum of per-address counts equals emails_filtered_total.
        let sum: u64 = snap.filtered_email_addresses.values().sum();
        assert_eq!(sum, snap.emails_filtered_total);
    }

    #[test]
    fn over_long_addresses_go_to_overflow() {
        let stats = Stats::new(None);
        let long = "a".repeat(MAX_ADDRESS_LEN + 1);
        stats.record_email_filtered(&format!("<{long}@example.com>"));
        let snap = stats.snapshot();
        assert_eq!(snap.emails_filtered_total, 1);
        assert_eq!(snap.filtered_email_addresses.get(OVERFLOW_KEY), Some(&1));
    }

    #[test]
    fn persist_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");

        let stats = Stats::new(Some(path.clone()));
        stats.record_request();
        stats.record_request();
        let body = br#"{"usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}}"#;
        stats.record_tokens_from_usage(body);
        stats.record_email_filtered("spam@example.com");
        stats.persist().unwrap();

        // Simulate restart.
        let reloaded = Stats::new(Some(path));
        let snap = reloaded.snapshot();
        assert_eq!(snap.requests_total, 2);
        assert_eq!(snap.prompt_tokens_total, 4);
        assert_eq!(snap.completion_tokens_total, 2);
        assert_eq!(snap.total_tokens_total, 6);
        assert_eq!(snap.emails_filtered_total, 1);
        assert_eq!(
            snap.filtered_email_addresses.get("spam@example.com"),
            Some(&1)
        );
    }

    #[test]
    fn persist_noop_when_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        let stats = Stats::new(Some(path.clone()));
        stats.persist().unwrap();
        assert!(
            !path.exists(),
            "no write should happen when dirty flag is false"
        );
    }
}
