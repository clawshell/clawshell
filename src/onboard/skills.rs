use super::types::{
    OPENCLAW_EMAIL_MESSAGES_SKILL_NAME, OnboardConfig, OnboardSkillBundle, OnboardSkillFile,
};

fn format_clawshell_base_url(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

pub fn render_openclaw_email_messages_skill(config: &OnboardConfig) -> Option<OnboardSkillBundle> {
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
        name: OPENCLAW_EMAIL_MESSAGES_SKILL_NAME,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::test_support::test_config;
    use crate::onboard::types::{OnboardEmailConfig, OnboardEmailMode};

    #[test]
    fn test_render_openclaw_email_messages_skill_returns_none_without_email() {
        let config = test_config();
        assert!(render_openclaw_email_messages_skill(&config).is_none());
    }

    #[test]
    fn test_render_openclaw_email_messages_skill_renders_concrete_values() {
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

        let skill = render_openclaw_email_messages_skill(&config).unwrap();
        assert_eq!(skill.name, OPENCLAW_EMAIL_MESSAGES_SKILL_NAME);
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
}
