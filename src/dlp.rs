use regex::bytes::Regex;
use tracing::{debug, trace};

use crate::config::{DlpAction, DlpPattern};

#[derive(Debug, Clone)]
pub struct CompiledPattern {
    pub name: String,
    pub regex: Regex,
    pub action: DlpAction,
}

#[derive(Debug, Clone)]
pub struct DlpScanner {
    patterns: Vec<CompiledPattern>,
    scan_responses: bool,
}

/// Result of scanning bytes for sensitive data.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// Pattern names that matched with action=block.
    pub blocked: Vec<String>,
    /// The bytes with action=redact patterns replaced.
    pub redacted: Vec<u8>,
    /// Whether any redaction was applied.
    pub was_redacted: bool,
}

impl DlpScanner {
    pub fn new(patterns: &[DlpPattern]) -> Result<Self, regex::Error> {
        Self::with_response_scanning(patterns, true)
    }

    pub fn with_response_scanning(
        patterns: &[DlpPattern],
        scan_responses: bool,
    ) -> Result<Self, regex::Error> {
        let compiled = patterns
            .iter()
            .map(|p| {
                trace!(name = %p.name, action = ?p.action, "Compiling DLP pattern");
                Ok(CompiledPattern {
                    name: p.name.clone(),
                    regex: Regex::new(&p.regex)?,
                    action: p.action,
                })
            })
            .collect::<Result<Vec<_>, regex::Error>>()?;
        debug!(
            pattern_count = compiled.len(),
            scan_responses, "DLP scanner initialized"
        );
        Ok(Self {
            patterns: compiled,
            scan_responses,
        })
    }

    /// Scans the input body for sensitive data.
    /// Returns a list of pattern names that matched with action=block (without including the actual sensitive data).
    #[cfg(test)]
    pub fn scan(&self, body: &[u8]) -> Vec<String> {
        trace!(body_len = body.len(), "Scanning body for block patterns");
        let matches: Vec<String> = self
            .patterns
            .iter()
            .filter(|p| p.action == DlpAction::Block && p.regex.is_match(body))
            .map(|p| {
                debug!(pattern = %p.name, "Block pattern matched");
                p.name.clone()
            })
            .collect();
        trace!(match_count = matches.len(), "Block scan complete");
        matches
    }

    /// Scans body and applies both block detection and redaction.
    /// Returns blocked pattern names and redacted body.
    pub fn scan_and_redact(&self, body: &[u8]) -> ScanResult {
        trace!(
            body_len = body.len(),
            "Scanning body for block+redact patterns"
        );
        let blocked: Vec<String> = self
            .patterns
            .iter()
            .filter(|p| p.action == DlpAction::Block && p.regex.is_match(body))
            .map(|p| {
                debug!(pattern = %p.name, "Block pattern matched in request");
                p.name.clone()
            })
            .collect();

        let mut redacted = body.to_vec();
        let mut was_redacted = false;

        for p in &self.patterns {
            if p.action == DlpAction::Redact && p.regex.is_match(&redacted) {
                debug!(pattern = %p.name, "Redact pattern matched, masking PII");
                let replacement = format!("[REDACTED:{}]", p.name);
                redacted = p
                    .regex
                    .replace_all(&redacted, replacement.as_bytes())
                    .to_vec();
                was_redacted = true;
            }
        }

        trace!(
            blocked_count = blocked.len(),
            was_redacted, "Scan-and-redact complete"
        );

        ScanResult {
            blocked,
            redacted,
            was_redacted,
        }
    }

    /// Redacts all sensitive data (both block and redact patterns) from body.
    /// Used for response scanning where we want to redact rather than block.
    pub fn redact_all(&self, body: &[u8]) -> (Vec<u8>, Vec<String>) {
        trace!(body_len = body.len(), "Redacting all patterns from body");
        let mut redacted = body.to_vec();
        let mut redacted_names = Vec::new();

        for p in &self.patterns {
            if p.regex.is_match(&redacted) {
                debug!(pattern = %p.name, "Pattern matched in response, redacting");
                let replacement = format!("[REDACTED:{}]", p.name);
                redacted = p
                    .regex
                    .replace_all(&redacted, replacement.as_bytes())
                    .to_vec();
                redacted_names.push(p.name.clone());
            }
        }

        trace!(redacted_count = redacted_names.len(), "Redact-all complete");
        (redacted, redacted_names)
    }

    /// Whether response scanning is enabled.
    pub fn scan_responses(&self) -> bool {
        self.scan_responses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DlpPattern;

    fn default_patterns() -> Vec<DlpPattern> {
        vec![
            DlpPattern {
                name: "credit_card".to_string(),
                regex: r"\b(?:\d[ -]*?){13,19}\b".to_string(),
                action: DlpAction::Block,
            },
            DlpPattern {
                name: "ssn".to_string(),
                regex: r"\b\d{3}-\d{2}-\d{4}\b".to_string(),
                action: DlpAction::Block,
            },
            DlpPattern {
                name: "email".to_string(),
                regex: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b".to_string(),
                action: DlpAction::Block,
            },
        ]
    }

    fn mixed_patterns() -> Vec<DlpPattern> {
        vec![
            DlpPattern {
                name: "ssn".to_string(),
                regex: r"\b\d{3}-\d{2}-\d{4}\b".to_string(),
                action: DlpAction::Block,
            },
            DlpPattern {
                name: "email".to_string(),
                regex: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b".to_string(),
                action: DlpAction::Redact,
            },
            DlpPattern {
                name: "phone_number".to_string(),
                regex: r"\b(?:\+?1[-.\s]?)?(?:\(?\d{3}\)?[-.\s]?)?\d{3}[-.\s]?\d{4}\b".to_string(),
                action: DlpAction::Redact,
            },
        ]
    }

    fn subslice(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn test_detect_credit_card() {
        let scanner = DlpScanner::new(&default_patterns()).unwrap();
        let matches = scanner.scan(b"My card is 4111 1111 1111 1111 please charge it");
        assert!(matches.contains(&"credit_card".to_string()));
    }

    #[test]
    fn test_detect_ssn() {
        let scanner = DlpScanner::new(&default_patterns()).unwrap();
        let matches = scanner.scan(b"My SSN is 123-45-6789");
        assert!(matches.contains(&"ssn".to_string()));
    }

    #[test]
    fn test_detect_email() {
        let scanner = DlpScanner::new(&default_patterns()).unwrap();
        let matches = scanner.scan(b"Contact me at user@example.com");
        assert!(matches.contains(&"email".to_string()));
    }

    #[test]
    fn test_no_sensitive_data() {
        let scanner = DlpScanner::new(&default_patterns()).unwrap();
        let matches = scanner.scan(b"Tell me about the weather today");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_multiple_detections() {
        let scanner = DlpScanner::new(&default_patterns()).unwrap();
        let matches = scanner.scan(b"Card: 4111111111111111, SSN: 123-45-6789, email: a@b.com");
        assert!(matches.contains(&"credit_card".to_string()));
        assert!(matches.contains(&"ssn".to_string()));
        assert!(matches.contains(&"email".to_string()));
    }

    #[test]
    fn test_empty_patterns() {
        let scanner = DlpScanner::new(&[]).unwrap();
        let matches = scanner.scan(b"4111111111111111");
        assert!(matches.is_empty());
    }

    // ========== Redaction Tests ==========

    #[test]
    fn test_redact_email() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        let result = scanner.scan_and_redact(b"Contact me at user@example.com");
        assert!(result.blocked.is_empty());
        assert!(result.was_redacted);
        assert!(subslice(&result.redacted, b"[REDACTED:email]"));
        assert!(!subslice(&result.redacted, b"user@example.com"));
    }

    #[test]
    fn test_redact_phone() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        let result = scanner.scan_and_redact(b"Call me at 555-123-4567");
        assert!(result.blocked.is_empty());
        assert!(result.was_redacted);
        assert!(subslice(&result.redacted, b"[REDACTED:phone_number]"));
        assert!(!subslice(&result.redacted, b"555-123-4567"));
    }

    #[test]
    fn test_block_ssn_and_redact_email() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        let result = scanner.scan_and_redact(b"SSN: 123-45-6789, email: user@example.com");
        assert!(result.blocked.contains(&"ssn".to_string()));
        assert!(result.was_redacted);
        assert!(subslice(&result.redacted, b"[REDACTED:email]"));
    }

    #[test]
    fn test_scan_only_returns_block_patterns() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        // Email is action=redact, so scan() should NOT return it
        let matches = scanner.scan(b"Contact me at user@example.com");
        assert!(!matches.contains(&"email".to_string()));
    }

    #[test]
    fn test_scan_returns_block_patterns() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        let matches = scanner.scan(b"My SSN is 123-45-6789");
        assert!(matches.contains(&"ssn".to_string()));
    }

    #[test]
    fn test_redact_all_replaces_everything() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        let (redacted, names) =
            scanner.redact_all(b"SSN: 123-45-6789, email: user@example.com, phone: 555-123-4567");
        assert!(names.contains(&"ssn".to_string()));
        assert!(names.contains(&"email".to_string()));
        assert!(names.contains(&"phone_number".to_string()));
        assert!(subslice(&redacted, b"[REDACTED:ssn]"));
        assert!(subslice(&redacted, b"[REDACTED:email]"));
        assert!(subslice(&redacted, b"[REDACTED:phone_number]"));
        assert!(!subslice(&redacted, b"123-45-6789"));
        assert!(!subslice(&redacted, b"user@example.com"));
        assert!(!subslice(&redacted, b"555-123-4567"));
    }

    #[test]
    fn test_redact_all_clean_bytes() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        let (redacted, names) = scanner.redact_all(b"Hello, how are you?");
        assert!(names.is_empty());
        assert_eq!(redacted, b"Hello, how are you?");
    }

    #[test]
    fn test_no_redaction_when_clean() {
        let scanner = DlpScanner::new(&mixed_patterns()).unwrap();
        let result = scanner.scan_and_redact(b"Hello world");
        assert!(result.blocked.is_empty());
        assert!(!result.was_redacted);
        assert_eq!(result.redacted, b"Hello world");
    }

    #[test]
    fn test_scan_responses_flag() {
        let scanner = DlpScanner::with_response_scanning(&[], true).unwrap();
        assert!(scanner.scan_responses());
        let scanner = DlpScanner::with_response_scanning(&[], false).unwrap();
        assert!(!scanner.scan_responses());
    }

    #[test]
    fn test_redact_multiple_occurrences() {
        let patterns = vec![DlpPattern {
            name: "email".to_string(),
            regex: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b".to_string(),
            action: DlpAction::Redact,
        }];
        let scanner = DlpScanner::new(&patterns).unwrap();
        let result = scanner.scan_and_redact(b"a@b.com and c@d.com");
        assert!(result.was_redacted);
        assert_eq!(result.redacted, b"[REDACTED:email] and [REDACTED:email]");
    }

    #[test]
    fn test_detect_phone_various_formats() {
        let patterns = vec![DlpPattern {
            name: "phone_number".to_string(),
            regex: r"\b(?:\+?1[-.\s]?)?(?:\(?\d{3}\)?[-.\s]?)?\d{3}[-.\s]?\d{4}\b".to_string(),
            action: DlpAction::Redact,
        }];
        let scanner = DlpScanner::new(&patterns).unwrap();

        // Standard US format
        let (redacted, names) = scanner.redact_all(b"Call 555-123-4567");
        assert!(names.contains(&"phone_number".to_string()));
        assert!(!subslice(&redacted, b"555-123-4567"));

        // With parentheses
        let (redacted, names) = scanner.redact_all(b"Call (555) 123-4567");
        assert!(names.contains(&"phone_number".to_string()));
        assert!(!subslice(&redacted, b"(555) 123-4567"));

        // With +1 prefix
        let (redacted, names) = scanner.redact_all(b"Call +1-555-123-4567");
        assert!(names.contains(&"phone_number".to_string()));
        assert!(!subslice(&redacted, b"+1-555-123-4567"));
    }

    #[test]
    fn test_non_utf8_input() {
        let patterns = vec![DlpPattern {
            name: "phone_number".to_string(),
            regex: r"\b(?:\+?1[-.\s]?)?(?:\(?\d{3}\)?[-.\s]?)?\d{3}[-.\s]?\d{4}\b".to_string(),
            action: DlpAction::Redact,
        }];
        let scanner = DlpScanner::new(&patterns).unwrap();
        let input = b"Call \xFF\xFE\xFD 555-123-4567";
        std::str::from_utf8(input.as_slice()).unwrap_err(); // Confirm it's not valid UTF-8

        let result = scanner.scan_and_redact(input);
        assert!(result.was_redacted);
        assert!(result.blocked.is_empty());
        assert_eq!(
            &result.redacted,
            b"Call \xFF\xFE\xFD [REDACTED:phone_number]"
        );

        let (redacted, names) = scanner.redact_all(input);
        assert!(names.contains(&"phone_number".to_string()));
        assert!(subslice(
            &redacted,
            b"Call \xFF\xFE\xFD [REDACTED:phone_number]"
        ));
    }
}
