use super::types::{
    ADMIN_STATS_SKILL_NAME, EMAIL_MESSAGES_SKILL_NAME, OnboardConfig, OnboardSkillBundle,
    OnboardSkillFile,
};

fn format_clawshell_base_url(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

pub fn render_email_messages_skill(config: &OnboardConfig) -> Option<OnboardSkillBundle> {
    config.email.as_ref()?;
    let base_url = format_clawshell_base_url(&config.server_host, config.server_port);

    let skill_md = format!(
        r#"---
name: get-email-messages
description: Get Email messages.
---

# Get Email Messages

Fetch Email message metadata and individual message content through the Email endpoint using
`curl`.

## Request

- Method: `GET`
- Path: `/v1/email/messages`
- Base URL: `{base_url}`
- Authorization: `Bearer <email_virtual_key>`

## Authentication Key Source

1. First, retrieve `email_virtual_key` from memory/context.
2. If not available, ask the user for the Email virtual key.
3. Ask whether they want you to remember this virtual key.
4. If yes, store `email_virtual_key` in memory/context; if no, use it only for the current request.

```bash
curl -sS \
  -H "Authorization: Bearer <email_virtual_key>" \
  "{base_url}/v1/email/messages"
```

## Optional Query Parameters

- `folder` (defaults to `INBOX`)
- `limit` (1-100)
- `unread_only` (`true`/`false`)
- `from`
- `subject`

## Response

Top-level fields:
- `messages`

## Get Individual Message Content

After listing messages, fetch one message by `id`:

```bash
curl -sS \
  -H "Authorization: Bearer <email_virtual_key>" \
  "{base_url}/v1/email/messages/42"
```

Content response fields:
- `metadata`
- `headers`
- `text_body`
- `html_body`

Load `references/api-usage.md` for detailed examples and status-code behavior.
"#
    );

    let reference_md = format!(
        r#"# GET /v1/email/messages API Usage

## Endpoint

- URL: `{base_url}/v1/email/messages`
- Header: `Authorization: Bearer <email_virtual_key>`

## Key Sourcing Order

1. Retrieve `email_virtual_key` from memory/context first.
2. Ask the user for it only if memory/context does not contain it.
3. Ask whether the user wants you to remember this virtual key.
4. Store `email_virtual_key` in memory/context only with explicit user consent; otherwise,
   use it only for the current request.

## Examples

### Basic request

```bash
curl -sS \
  -H "Authorization: Bearer <email_virtual_key>" \
  "{base_url}/v1/email/messages"
```

### Filter unread from trusted.local

```bash
curl -sS \
  -H "Authorization: Bearer <email_virtual_key>" \
  --get "{base_url}/v1/email/messages" \
  --data-urlencode "folder=INBOX" \
  --data-urlencode "limit=25" \
  --data-urlencode "unread_only=true" \
  --data-urlencode "from=@trusted.local" \
  --data-urlencode "subject=invoice"
```

### Fetch a message's full content

```bash
curl -sS \
  -H "Authorization: Bearer <email_virtual_key>" \
  "{base_url}/v1/email/messages/42"
```

Expected top-level fields:
- `metadata`
- `headers`
- `text_body`
- `html_body`

## Notes

- `limit` must be between 1 and 100.
- Error payloads are JSON objects: `{{"error":"message"}}`.
"#
    );

    Some(OnboardSkillBundle {
        name: EMAIL_MESSAGES_SKILL_NAME,
        files: vec![
            OnboardSkillFile {
                relative_path: "SKILL.md",
                content: skill_md,
            },
            OnboardSkillFile {
                relative_path: "references/api-usage.md",
                content: reference_md,
            },
        ],
    })
}

/// Render the `get-clawshell-stats` skill that teaches the downstream agent
/// how to fetch `GET /admin/stats` and report the result to the user.
///
/// Always returns a bundle — the endpoint is available on every ClawShell
/// install, so there's no gating helper and no `Option` wrapper.
pub fn render_admin_stats_skill(config: &OnboardConfig) -> OnboardSkillBundle {
    let base_url = format_clawshell_base_url(&config.server_host, config.server_port);

    let skill_md = format!(
        r#"---
name: get-clawshell-stats
description: Fetch ClawShell runtime stats and report them to the user.
---

# Get ClawShell Stats

Fetch aggregate runtime counters from ClawShell's management endpoint and
summarize them for the user.

## Request

- Method: `GET`
- Path: `/admin/stats`
- Base URL: `{base_url}`
- Auth: none — the endpoint is reachable only from the loopback interface
  (`127.0.0.1` / `::1`) and rejects any non-loopback peer with `403`.

```bash
curl -sS "{base_url}/admin/stats"
```

## Response shape

```json
{{
  "requests_total": 1234,
  "prompt_tokens_total": 500000,
  "completion_tokens_total": 120000,
  "total_tokens_total": 620000,
  "emails_filtered_total": 42,
  "filtered_email_addresses": {{
    "spam@example.com": 37,
    "phish@bad.example": 5
  }}
}}
```

## Reporting to the user

After the request succeeds, present a short human-readable summary:

1. Total requests served and total tokens (prompt + completion + combined).
2. Email-filter activity: the total filtered count, plus the top 5 addresses
   by per-address count.
3. If the `filtered_email_addresses` map contains the synthetic key
   `<overflow>`, mention it separately as "N filtered senders past the
   tracking cap" — do not present it as a real sender address.
4. If `requests_total` is 0, say "no traffic since the last reset" rather
   than dumping a block of zeros.

Load `references/api-usage.md` for error handling and edge cases.
"#
    );

    let reference_md = format!(
        r#"# GET /admin/stats API Usage

## Endpoint

- URL: `{base_url}/admin/stats`
- Auth: none. The handler checks the peer IP and returns `403 Forbidden`
  for any non-loopback client, so the skill is only usable when the
  downstream agent runs on the same host as ClawShell.

## Example

```bash
curl -sS "{base_url}/admin/stats"
```

## Full response schema

- `requests_total` (u64): every request that reached the axum router,
  regardless of status code. Includes both the proxy catch-all and the
  `/v1/email/*` routes (and this `/admin/stats` route itself).
- `prompt_tokens_total` (u64): sum of upstream `prompt_tokens` /
  `input_tokens` values parsed from **non-streaming** JSON responses.
- `completion_tokens_total` (u64): sum of `completion_tokens` /
  `output_tokens`, same caveat.
- `total_tokens_total` (u64): sum of `total_tokens`, or
  `prompt_tokens + completion_tokens` when the upstream didn't include
  an explicit total.
- `emails_filtered_total` (u64): count of times the email sender policy
  hid a message from the downstream result.
- `filtered_email_addresses` (object: string → u64): per-address count
  of times that sender was hidden. The sum of the values equals
  `emails_filtered_total`.

## Caveats & edge cases

- **SSE streams are not counted** in token totals. If most of the traffic
  is streaming completions, `*_tokens_total` will under-report real
  upstream usage. Mention this if the user asks why the token numbers
  look low.
- **`<overflow>` key**: the filtered-address map is hard-capped at
  10,000 unique senders. Once the cap is hit, any further unique
  addresses are aggregated under a synthetic `<overflow>` key. The
  `<overflow>` entry's value is the count of *additional* unique senders
  the map couldn't track individually — treat it as a bounded counter,
  not a real sender.
- **Counters are in-memory + periodically persisted**. If ClawShell was
  restarted very recently, small numbers are expected. Don't confuse
  "recent restart" with "low traffic".
- **Loopback check**: if you get `403 Forbidden`, it means the request
  reached ClawShell from a non-loopback source. That usually indicates
  the downstream agent is running on a different host than ClawShell
  and the skill is not applicable in that deployment.

## Error payloads

Error responses are JSON objects shaped as `{{"error":"message"}}`.
"#
    );

    OnboardSkillBundle {
        name: ADMIN_STATS_SKILL_NAME,
        files: vec![
            OnboardSkillFile {
                relative_path: "SKILL.md",
                content: skill_md,
            },
            OnboardSkillFile {
                relative_path: "references/api-usage.md",
                content: reference_md,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::test_support::test_config;
    use crate::onboard::types::{OnboardEmailConfig, OnboardEmailMode};

    #[test]
    fn test_render_email_messages_skill_returns_none_without_email() {
        let config = test_config();
        assert!(render_email_messages_skill(&config).is_none());
    }

    #[test]
    fn test_render_email_messages_skill_renders_concrete_values() {
        let mut config = test_config();
        config.email = Some(OnboardEmailConfig {
            mode: OnboardEmailMode::Allowlist,
            sender_rules: vec!["@trusted.local".to_string()],
            account_virtual_key: "vk-email-001".to_string(),
            email: "bot@gmail.com".to_string(),
            app_password: "abcd efgh ijkl mnop".to_string(),
            imap_host: "imap.gmail.com".to_string(),
            imap_port: 993,
        });

        let skill = render_email_messages_skill(&config).unwrap();
        assert_eq!(skill.name, EMAIL_MESSAGES_SKILL_NAME);
        assert_eq!(skill.files.len(), 2);

        let skill_md = skill
            .files
            .iter()
            .find(|file| file.relative_path == "SKILL.md")
            .unwrap()
            .content
            .as_str();
        assert!(skill_md.contains("http://127.0.0.1:18790"));
        assert!(skill_md.contains("Bearer <email_virtual_key>"));
        assert!(skill_md.contains("First, retrieve `email_virtual_key` from memory/context."));
        assert!(skill_md.contains("If not available, ask the user for the Email virtual key."));
        assert!(skill_md.contains("Ask whether they want you to remember this virtual key."));
        assert!(skill_md
            .contains("If yes, store `email_virtual_key` in memory/context; if no, use it only for the current request."));
        assert!(skill_md.contains("/v1/email/messages/42"));
        assert!(skill_md.contains("html_body"));
        assert!(!skill_md.contains("vk-email-001"));
        assert!(!skill_md.contains("CLAWSHELL_BASE_URL"));
        assert!(!skill_md.contains("VIRTUAL_KEY"));

        let reference_md = skill
            .files
            .iter()
            .find(|file| file.relative_path == "references/api-usage.md")
            .unwrap()
            .content
            .as_str();
        assert!(reference_md.contains("trusted.local"));
        assert!(reference_md.contains("/v1/email/messages/42"));
        assert!(reference_md.contains("text_body"));
        assert!(reference_md.contains("Bearer <email_virtual_key>"));
        assert!(reference_md.contains("Retrieve `email_virtual_key` from memory/context first."));
        assert!(
            reference_md.contains("Ask whether the user wants you to remember this virtual key.")
        );
        assert!(reference_md.contains(
            "Store `email_virtual_key` in memory/context only with explicit user consent;"
        ));
        assert!(!reference_md.contains("vk-email-001"));
    }

    #[test]
    fn test_render_admin_stats_skill_has_both_files() {
        let config = test_config();
        let skill = render_admin_stats_skill(&config);
        assert_eq!(skill.name, ADMIN_STATS_SKILL_NAME);
        assert_eq!(skill.files.len(), 2);
        assert!(
            skill
                .files
                .iter()
                .any(|file| file.relative_path == "SKILL.md")
        );
        assert!(
            skill
                .files
                .iter()
                .any(|file| file.relative_path == "references/api-usage.md")
        );
    }

    #[test]
    fn test_render_admin_stats_skill_without_email_still_renders() {
        // Unlike the email skill, the stats skill must not be gated on
        // email configuration — stats are available on every ClawShell
        // install.
        let config = test_config();
        assert!(config.email.is_none());
        let skill = render_admin_stats_skill(&config);
        assert_eq!(skill.name, ADMIN_STATS_SKILL_NAME);
    }

    #[test]
    fn test_render_admin_stats_skill_renders_concrete_values() {
        let config = test_config();
        let skill = render_admin_stats_skill(&config);

        let skill_md = skill
            .files
            .iter()
            .find(|file| file.relative_path == "SKILL.md")
            .unwrap()
            .content
            .as_str();
        assert!(skill_md.contains("http://127.0.0.1:18790"));
        assert!(skill_md.contains("/admin/stats"));
        assert!(skill_md.contains("Reporting to the user"));
        assert!(skill_md.contains("top 5 addresses"));
        assert!(skill_md.contains("<overflow>"));
        // No auth — the endpoint is loopback-only.
        assert!(!skill_md.contains("Authorization"));
        assert!(!skill_md.contains("Bearer"));
        assert!(!skill_md.contains("virtual_key"));

        let reference_md = skill
            .files
            .iter()
            .find(|file| file.relative_path == "references/api-usage.md")
            .unwrap()
            .content
            .as_str();
        assert!(reference_md.contains("http://127.0.0.1:18790/admin/stats"));
        assert!(reference_md.contains("SSE streams are not counted"));
        assert!(reference_md.contains("`<overflow>` key"));
        assert!(reference_md.contains("403 Forbidden"));
        assert!(!reference_md.contains("Authorization"));
        assert!(!reference_md.contains("Bearer"));
    }
}
