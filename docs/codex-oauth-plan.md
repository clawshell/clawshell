# Multi-Provider OAuth Integration Plan for ClawShell

## Executive Summary

This document proposes adding a **multi-provider OAuth framework** to ClawShell,
enabling users to authenticate with subscription-based accounts instead of (or
alongside) static API keys.  The first version implements two OAuth providers:
**Codex (OpenAI)** and **Antigravity (Google)**.

OAuth providers are integrated into the existing `clawshell onboard` wizard —
no new CLI subcommands are needed.  Users select a provider from the same menu
that already shows OpenAI, OpenRouter, and Anthropic.

### Provider Roadmap

| Provider                  | Status           | Upstream API                         | Auth Via                      |
|---------------------------|------------------|--------------------------------------|-------------------------------|
| **Codex (OpenAI)**        | v1 — Implement   | `api.openai.com`                     | ChatGPT Plus/Pro subscription |
| **Antigravity (Google)**  | v1 — Implement   | `cloudcode-pa.googleapis.com`        | Google account                |
| **Claude (Anthropic)**    | Blocked by ToS   | `api.anthropic.com`                  | Claude Pro/Max subscription   |

> **Note on Claude OAuth:** Anthropic explicitly banned OAuth token usage in
> third-party tools as of January 2026. Their updated "Authentication and credential
> use" policy states that OAuth tokens from Free, Pro, and Max plans are authorized
> **exclusively for Claude Code and Claude.ai**. This provider cannot be implemented
> until Anthropic changes their policy. See Section 3.5.

> **Note on Antigravity ToS:** There are reports of Google blocking accounts using
> third-party Antigravity auth plugins. The Antigravity ToS (as of 2026-02-18) states
> their service cannot be used with third-party products. This risk is documented in
> Section 10 but does not block implementation.

---

## 1. OAuth Providers — Technical Details

### 1.1 Codex OAuth (OpenAI)

OpenAI's OAuth 2.0 + PKCE flow used by the Codex CLI to authenticate ChatGPT
subscribers.

| Parameter              | Value                                                 |
|------------------------|-------------------------------------------------------|
| Authorization endpoint | `https://auth.openai.com/authorize`                   |
| Token endpoint         | `https://auth.openai.com/oauth/token`                 |
| Client ID              | `app_EMoamEEZ73f0CkXaXp7hrann`                       |
| Redirect URI           | `http://localhost:<port>/auth/callback`                |
| Scopes                 | `openid profile email offline_access`                 |
| PKCE method            | S256                                                  |
| Token refresh interval | ~8 days                                               |
| Device code flow       | Supported                                             |
| Token injection        | `Authorization: Bearer <access_token>`                |
| API format             | OpenAI-native (pass-through, no body transformation)  |

**Tokens produced:** access token (short-lived JWT), refresh token (long-lived,
single-use), ID token (user identity claims).

### 1.2 Antigravity OAuth (Google)

Google's OAuth 2.0 + PKCE flow used by the Antigravity IDE to authenticate Google
account holders.

| Parameter              | Value                                                        |
|------------------------|--------------------------------------------------------------|
| Authorization endpoint | `https://accounts.google.com/o/oauth2/auth`                 |
| Token endpoint         | `https://oauth2.googleapis.com/token`                        |
| Client ID              | Antigravity OAuth client (configurable)                      |
| Redirect URI           | `http://localhost:<port>/oauth-callback`                     |
| Scopes                 | `openid profile email https://www.googleapis.com/auth/cloud-platform` |
| Additional scopes      | `auth/cclog`, `auth/experimentsandconfigs`                   |
| Access type            | `offline` (enables refresh token)                            |
| PKCE method            | S256                                                         |
| Prompt                 | `consent` (forces consent screen)                            |
| Device code flow       | No — headless fallback via copy/paste URL                    |
| Token injection        | `Authorization: Bearer <access_token>` + extra headers       |
| API format             | Gemini-style (requires request body wrapping)                |

**Tokens produced:** access token, refresh token, with associated project_id and
account metadata.

**API Endpoints (with fallback):**

| Tier        | Base URL                                                       |
|-------------|----------------------------------------------------------------|
| Production  | `https://cloudcode-pa.googleapis.com`                          |
| Daily       | `https://daily-cloudcode-pa.sandbox.googleapis.com`            |
| Alt Prod    | `https://codeassist.googleapis.com/v1`                         |

**Required Headers (beyond Bearer token):**
- `X-Goog-Api-Client: google-cloud-sdk vscode_cloudshelleditor/0.1`
- `Client-Metadata: {"ideType":"ANTIGRAVITY","platform":"<OS>","pluginType":"GEMINI"}`
- `User-Agent: antigravity/1.15.8 <platform>`

**Key paths:**
- Generate content: `/v1internal:generateContent`
- Streaming: `/v1internal:streamGenerateContent?alt=sse`

**Token storage fields:** `email`, `accessToken`, `refreshToken`, `expiresAt`,
`projectId`, `tier`, `rateLimitedUntil`, `lastUsed`.

**Available models:** Gemini 3 Pro/Flash, Claude Sonnet 4.6, Claude Opus 4.6
(Thinking), GPT-OSS 120B.

### 1.3 Key Differences Between Providers

| Aspect                | Codex (OpenAI)                    | Antigravity (Google)                     |
|-----------------------|-----------------------------------|------------------------------------------|
| Auth server           | `auth.openai.com`                 | `accounts.google.com`                    |
| Token endpoint        | `auth.openai.com/oauth/token`     | `oauth2.googleapis.com/token`            |
| API request format    | OpenAI-native                     | Gemini-style (wrapped body)              |
| Extra headers needed  | None                              | `X-Goog-Api-Client`, `Client-Metadata`  |
| Upstream routing      | Single endpoint                   | 3-tier fallback                          |
| Project ID            | Not required                      | Required (per-account `projectId`)       |
| Headless support      | Device code (interactive)         | Copy/paste URL (manual)                  |
| Multi-account         | Single account                    | Up to 10 accounts with rotation          |
| Token refresh check   | Background (75% TTL)              | Pre-request (60s before expiry)          |
| Request body changes  | Pass-through                      | Wrap with project metadata               |

### 1.4 Claude OAuth (Anthropic) — Blocked

**Not implementable.** Anthropic deployed a technical block on January 9, 2026
rejecting all OAuth tokens from non-Claude-Code clients. Policy formalized
~February 17-18, 2026. Error: "This credential is only authorized for use with
Claude Code."

---

## 2. Why Add Multi-Provider OAuth?

| Benefit                         | Detail                                                          |
|---------------------------------|-----------------------------------------------------------------|
| No API key required             | Users with subscriptions can use their existing accounts        |
| Broader user base               | Many users have subscriptions but not API keys                  |
| Better security                 | OAuth tokens are short-lived and revocable vs. static keys      |
| Multi-model access              | Antigravity gives access to Gemini, Claude, and GPT-OSS models |
| Cost savings                    | Subscription usage avoids separate per-token API charges        |

---

## 3. Feasibility Assessment

### 3.1 Compatible — Low Risk

| Aspect              | Why it works                                                       |
|----------------------|--------------------------------------------------------------------|
| HTTP proxy model     | ClawShell already intercepts and rewrites auth headers             |
| Provider abstraction | `Provider` enum and `ProxyClient` already branch on provider type  |
| Config system        | TOML config is extensible — add `[[oauth_providers]]` table        |
| Onboard wizard       | Already has a provider selection menu — just add new entries       |
| Rust ecosystem       | `oauth2` crate handles PKCE; `open` for browser; `reqwest` for API |
| Daemon architecture  | Token refresh can run as background `tokio` tasks                  |

### 3.2 Challenges

| Challenge                          | Mitigation                                                     |
|------------------------------------|----------------------------------------------------------------|
| Browser needed for initial login   | Headless fallback for both providers                           |
| Token storage security             | Store in `/etc/clawshell/oauth/` with 0600 perms              |
| Token refresh in a daemon          | Background tokio task per provider; proactive refresh          |
| Provider-specific API formats      | Trait-based abstraction with `prepare_request()` per provider  |
| Antigravity request body wrapping  | `AntigravityProvider` handles Gemini-style body transformation |
| Client ID stability                | All OAuth parameters configurable per provider                 |
| ToS restrictions                   | Monitor each provider's policy; disable if 3P banned           |

### 3.3 Open Questions — Codex (v1)

1. Does OpenAI's API accept ChatGPT OAuth access tokens on the standard
   `/v1/chat/completions` and `/v1/responses` endpoints?
2. Are there rate limits or model restrictions specific to OAuth-authenticated requests?
3. Is the Codex client ID (`app_EMoamEEZ73f0CkXaXp7hrann`) stable for third-party use?

### 3.4 Open Questions — Antigravity (v1)

1. Does the Antigravity API return standard error codes or Google-specific ones?
2. What is the exact token TTL (for refresh scheduling)?
3. What `projectId` assignment flow is needed on first login?
4. Are there per-account rate limits beyond what the plugin documents?

### 3.5 Claude OAuth — Status

**Not implementable.** Anthropic deployed a technical block on January 9, 2026.
Policy updated ~February 17-18, 2026. There is no known workaround.

---

## 4. Architecture

### 4.1 Current Flow (API Key Only)

```
OpenClaw ──► ClawShell (virtual key → real API key) ──► OpenAI / Anthropic API
```

### 4.2 Proposed Flow (Multi-Provider OAuth)

```
              clawshell onboard
              ┌──────────────────────────────────────────┐
              │                                          │
              │  Select a model provider:                │
              │    1. OpenAI              (API key)      │  ← existing
              │    2. OpenRouter          (API key)      │  ← existing
              │    3. Anthropic           (API key)      │  ← existing
              │    4. Codex / ChatGPT     (OAuth login)  │  ← NEW
              │    5. Antigravity / Google (OAuth login)  │  ← NEW
              │                                          │
              │  If 1-3: prompt for API key (unchanged)  │
              │  If 4:   open browser → auth.openai.com  │
              │  If 5:   open browser → accounts.google  │
              │                                          │
              └──────────────────────────────────────────┘

                         RUNTIME
┌──────────┐    Bearer vk-001    ┌───────────────────────────┐
│          │ ──────────────────► │        ClawShell           │
│ OpenClaw │                    │                             │
│          │ ◄────────────────── │  Lookup vk-001 → KeySource │
└──────────┘     response       │         │                   │
                                │    ┌────┴────┐              │
                                │ Static    OAuth{id}         │
                                │    │         │              │
                                │    ▼         ▼              │
                                │ real_key  OAuthRegistry     │
                                │    │     ┌──────────────┐   │
                                │    │     │ provider_id?  │   │
                                │    │     │  codex ──────►│───│──► api.openai.com
                                │    │     │  antigravity─►│───│──► cloudcode-pa.googleapis.com
                                │    │     └──────────────┘   │
                                │    └────┬────┘              │
                                │         ▼                   │
                                │   Forward to upstream       │
                                └─────────────────────────────┘

                         BACKGROUND
                ┌───────────────────────────────┐
                │  Refresh Task: codex           │  sleep(75% of TTL)
                ├───────────────────────────────┤
                │  Refresh Task: antigravity     │  check 60s before expiry
                └───────────────────────────────┘
```

### 4.3 Module Map

```
src/
├── oauth/
│   ├── mod.rs           ← NEW: OAuthProvider trait, OAuthRegistry, shared types
│   ├── codex.rs         ← NEW: Codex (OpenAI) provider                   [v1]
│   ├── antigravity.rs   ← NEW: Antigravity (Google) provider             [v1]
│   └── storage.rs       ← NEW: Per-provider token persistence
├── lib.rs               ← MODIFY: AppState gains OAuthRegistry
├── cli.rs               ← UNCHANGED (no new subcommands)
├── config.rs            ← MODIFY: add [[oauth_providers]] config section
├── keys.rs              ← MODIFY: ResolvedKey gains OAuth{provider_id}
├── proxy.rs             ← MODIFY: provider.inject_auth() + prepare_request() + 401-retry
├── main.rs              ← MODIFY: initialize OAuthRegistry, start refresh tasks
├── onboard/
│   ├── interactive.rs   ← MODIFY: add OAuth providers to menu, run OAuth flow
│   ├── types.rs         ← MODIFY: OnboardConfig supports OAuth auth method
│   ├── config_render.rs ← MODIFY: render [[oauth_providers]] + auth="oauth" in TOML
│   └── (rest unchanged)
└── (rest unchanged)
```

---

## 5. Detailed Design

### 5.1 The `OAuthProvider` Trait

The core abstraction enabling multiple providers:

```rust
#[async_trait]
pub trait OAuthProvider: Send + Sync + std::fmt::Debug {
    /// Unique identifier (e.g., "codex", "antigravity").
    fn id(&self) -> &str;

    /// Display name (e.g., "Codex (OpenAI)", "Antigravity (Google)").
    fn display_name(&self) -> &str;

    /// Execute browser-based OAuth login flow.
    async fn login_browser(&self, callback_port: u16) -> Result<OAuthTokens, OAuthError>;

    /// Execute headless login flow (device code or copy/paste URL).
    async fn login_headless(&self) -> Result<OAuthTokens, OAuthError>;

    /// Refresh the access token using the refresh token.
    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, OAuthError>;

    /// Inject provider-specific auth headers into the request.
    fn inject_auth(&self, headers: &mut HeaderMap, access_token: &str) -> Result<(), OAuthError>;

    /// Optionally transform the request body for provider-specific formats.
    /// Returns None for pass-through (Codex); Some(wrapped) for Antigravity.
    fn prepare_request_body(
        &self, body: &[u8], tokens: &OAuthTokens,
    ) -> Result<Option<Vec<u8>>, OAuthError> {
        let _ = (body, tokens);
        Ok(None)
    }

    /// Resolve the upstream URL for this provider.
    /// Returns None to use the configured [upstream] URL (Codex).
    fn upstream_url(&self, tokens: &OAuthTokens) -> Option<String> {
        let _ = tokens;
        None
    }

    /// Whether this provider supports device code flow.
    fn supports_device_code(&self) -> bool { false }

    /// Whether this provider supports headless copy/paste URL fallback.
    fn supports_headless_url(&self) -> bool { false }
}
```

### 5.2 Codex Provider (`codex.rs`)

```rust
#[derive(Debug)]
pub struct CodexProvider {
    client_id: String,
    auth_url: String,
    token_url: String,
    scopes: Vec<String>,
    http_client: reqwest::Client,
}

impl OAuthProvider for CodexProvider {
    fn id(&self) -> &str { "codex" }
    fn display_name(&self) -> &str { "Codex (OpenAI)" }
    fn supports_device_code(&self) -> bool { true }

    fn inject_auth(&self, headers: &mut HeaderMap, token: &str) -> Result<(), OAuthError> {
        headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse()?);
        Ok(())
    }
    // prepare_request_body: default (None — pass-through)
    // upstream_url: default (None — use [upstream].base_url)
}
```

### 5.3 Antigravity Provider (`antigravity.rs`)

```rust
#[derive(Debug)]
pub struct AntigravityProvider {
    client_id: String,
    auth_url: String,
    token_url: String,
    scopes: Vec<String>,
    http_client: reqwest::Client,
    endpoints: Vec<String>,
}

impl OAuthProvider for AntigravityProvider {
    fn id(&self) -> &str { "antigravity" }
    fn display_name(&self) -> &str { "Antigravity (Google)" }
    fn supports_headless_url(&self) -> bool { true }

    fn inject_auth(&self, headers: &mut HeaderMap, token: &str) -> Result<(), OAuthError> {
        headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse()?);
        headers.insert("x-goog-api-client",
            "google-cloud-sdk vscode_cloudshelleditor/0.1".parse()?);
        headers.insert("client-metadata",
            r#"{"ideType":"ANTIGRAVITY","platform":"LINUX","pluginType":"GEMINI"}"#.parse()?);
        Ok(())
    }

    fn prepare_request_body(&self, body: &[u8], tokens: &OAuthTokens)
        -> Result<Option<Vec<u8>>, OAuthError> {
        let project_id = tokens.extra.get("project_id")
            .and_then(|v| v.as_str())
            .ok_or(OAuthError::LoginFailed("Missing project_id".into()))?;
        let wrapped = wrap_antigravity_request(body, project_id)?;
        Ok(Some(wrapped))
    }

    fn upstream_url(&self, _tokens: &OAuthTokens) -> Option<String> {
        Some(format!("{}/v1internal:streamGenerateContent?alt=sse",
            self.endpoints.last().unwrap_or(&self.endpoints[0])))
    }
}
```

### 5.4 `OAuthRegistry`

```rust
#[derive(Debug)]
pub struct OAuthRegistry {
    providers: BTreeMap<String, Arc<dyn OAuthProvider>>,
    tokens: Arc<RwLock<BTreeMap<String, OAuthTokens>>>,
    storage: TokenStorage,
}

impl OAuthRegistry {
    pub async fn current_access_token(&self, provider_id: &str) -> Result<String, OAuthError>;
    pub async fn inject_auth(&self, provider_id: &str, headers: &mut HeaderMap)
        -> Result<(), OAuthError>;
    pub async fn prepare_request_body(&self, provider_id: &str, body: &[u8])
        -> Result<Option<Vec<u8>>, OAuthError>;
    pub async fn upstream_url(&self, provider_id: &str) -> Result<Option<String>, OAuthError>;
    pub async fn refresh(&self, provider_id: &str) -> Result<(), OAuthError>;
    pub fn spawn_refresh_tasks(&self, cancel: CancellationToken);
}
```

### 5.5 Token Storage (`storage.rs`)

Per-provider token files under `/etc/clawshell/oauth/`:

```
/etc/clawshell/oauth/
├── codex.json
│   { "access_token": "...", "refresh_token": "...", "expires_at": "...",
│     "account_id": "...", "extra": {} }
└── antigravity.json
    { "access_token": "...", "refresh_token": "...", "expires_at": "...",
      "account_id": "user@gmail.com",
      "extra": { "project_id": "...", "tier": "...", "email": "..." } }
```

All files mode 0600, directory mode 0700, owned by `clawshell` user.

### 5.6 Onboarding Changes (`onboard/interactive.rs`)

The existing provider menu at line 326-331 currently shows:

```rust
let provider_options = match existing.provider.as_deref() {
    Some("anthropic") => vec!["Anthropic", "OpenAI", "OpenRouter"],
    Some("openrouter") => vec!["OpenRouter", "OpenAI", "Anthropic"],
    _ => vec!["OpenAI", "OpenRouter", "Anthropic"],
};
```

This becomes:

```rust
let provider_options = match existing.provider.as_deref() {
    Some("anthropic") => vec!["Anthropic", "OpenAI", "OpenRouter",
                              "Codex / ChatGPT (OAuth)", "Antigravity / Google (OAuth)"],
    Some("codex") => vec!["Codex / ChatGPT (OAuth)", "OpenAI", "OpenRouter",
                          "Anthropic", "Antigravity / Google (OAuth)"],
    Some("antigravity") => vec!["Antigravity / Google (OAuth)", "OpenAI", "OpenRouter",
                                "Anthropic", "Codex / ChatGPT (OAuth)"],
    // ...
    _ => vec!["OpenAI", "OpenRouter", "Anthropic",
              "Codex / ChatGPT (OAuth)", "Antigravity / Google (OAuth)"],
};
```

**After provider selection**, the flow branches:

```
If provider is "openai" | "openrouter" | "anthropic":
    → existing flow: prompt for API key, model, virtual key, etc.

If provider is "codex":
    → prompt for model name (default: models available via ChatGPT)
    → detect headless environment (SSH_CONNECTION, etc.)
    → if headless: run device code flow
    → else: run browser PKCE flow → auth.openai.com
    → store tokens to /etc/clawshell/oauth/codex.json
    → prompt for virtual key
    → continue with OpenClaw config, server settings, etc.

If provider is "antigravity":
    → prompt for model name (default: gemini-3-pro)
    → detect headless environment
    → if headless: print auth URL, prompt for redirect URL paste
    → else: run browser PKCE flow → accounts.google.com
    → discover project_id via loadCodeAssist
    → store tokens to /etc/clawshell/oauth/antigravity.json
    → prompt for virtual key
    → continue with OpenClaw config, server settings, etc.
```

**The real API key prompt is skipped entirely for OAuth providers.** The
`OnboardConfig` struct changes:

```rust
pub enum AuthMethod {
    ApiKey { real_api_key: String },
    OAuth { provider_id: String },       // tokens already stored by the onboard flow
}

pub struct OnboardConfig {
    pub provider: String,
    pub model: String,
    pub auth: AuthMethod,                // was: pub real_api_key: String
    pub virtual_api_key: String,
    pub openclaw_config_path: PathBuf,
    pub server_host: String,
    pub server_port: u16,
    pub email: Option<OnboardEmailConfig>,
}
```

**Re-onboard behavior:** When a user runs `clawshell onboard` again and a previous
OAuth config exists, the wizard detects it (from `config.json` and the presence of
token files) and offers to re-authenticate or keep the existing tokens.

### 5.7 Config Rendering Changes (`onboard/config_render.rs`)

When the user selects an OAuth provider, the generated `clawshell.toml` includes:

```toml
[[keys]]
virtual_key = "vk-chatgpt-001"
provider = "openai"
auth = "oauth"
oauth_provider = "codex"

[[oauth_providers]]
provider = "codex"
```

And `config.json` stores `"provider": "codex"` (or `"antigravity"`) for re-onboard
detection.

### 5.8 Config Changes (`config.rs`)

```rust
pub struct Config {
    pub server: ServerConfig,
    pub upstream: UpstreamConfig,
    pub keys: Vec<KeyMapping>,
    pub dlp: DlpConfig,
    pub log_level: String,
    #[serde(default)]
    pub oauth_providers: Vec<OAuthProviderConfig>,
}

pub struct KeyMapping {
    pub virtual_key: String,
    pub real_key: Option<String>,       // optional when auth = "oauth"
    pub provider: Provider,
    #[serde(default)]
    pub auth: AuthMethod,               // defaults to Static
    pub oauth_provider: Option<String>, // "codex" or "antigravity"
}

#[derive(Default)]
pub enum AuthMethod { #[default] Static, OAuth }

pub struct OAuthProviderConfig {
    pub provider: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub client_id: Option<String>,
    pub auth_url: Option<String>,
    pub token_url: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub callback_port: Option<u16>,
}
```

### 5.9 Key Resolution Changes (`keys.rs`)

```rust
pub enum KeySource {
    Static(String),
    OAuth { provider_id: String },
}

pub struct ResolvedKey {
    pub source: KeySource,
    pub provider: Provider,
}
```

### 5.10 Proxy Changes (`proxy.rs`)

```rust
match resolved.source {
    KeySource::Static(ref key) => {
        // existing logic
    }
    KeySource::OAuth { ref provider_id } => {
        oauth_registry.inject_auth(provider_id, &mut req_headers).await?;
        let body = match oauth_registry.prepare_request_body(provider_id, &body).await? {
            Some(transformed) => Bytes::from(transformed),
            None => body,
        };
        let upstream = match oauth_registry.upstream_url(provider_id).await? {
            Some(url) => url,
            None => default_upstream_url(provider),
        };
        // send, handle 401 → refresh + retry once
    }
}
```

### 5.11 AppState Changes (`lib.rs`)

```rust
pub struct AppState {
    pub key_manager: Arc<KeyManager>,
    pub dlp_scanner: Arc<DlpScanner>,
    pub proxy_client: Arc<ProxyClient>,
    pub oauth_registry: Option<Arc<OAuthRegistry>>,
}
```

---

## 6. New Dependencies

| Crate         | Purpose                                      | Size Impact |
|---------------|----------------------------------------------|-------------|
| `oauth2`      | OAuth 2.0 client with PKCE support           | Moderate    |
| `open`        | Open browser for auth URL (cross-platform)   | Tiny        |
| `chrono`      | Token expiry math                            | Small       |
| `base64`      | PKCE verifier encoding (may be transitive)   | Tiny        |
| `async-trait` | Trait async methods (if not Rust 1.85+)      | Small       |

---

## 7. Security Considerations

| Concern                    | Mitigation                                                       |
|----------------------------|------------------------------------------------------------------|
| Token files on disk        | Per-provider files in `/etc/clawshell/oauth/` with 0600 perms    |
| Token in memory            | `Arc<RwLock<...>>` — same threat model as current keys           |
| Refresh token theft        | Single-use rotation (Codex); standard rotation (Antigravity)     |
| PKCE                       | Both providers use S256 — prevents code interception             |
| Callback server exposure   | `127.0.0.1` only; ephemeral; shuts down after one use            |
| Provider client IDs        | Configurable per provider in `[[oauth_providers]]`               |
| ToS compliance             | Monitor each provider's policy; documented risks                 |
| Provider isolation         | Separate token files — compromise of one doesn't affect others   |
| Antigravity extra headers  | Injected server-side; client never sees them                     |

---

## 8. Testing Strategy

| Layer        | Approach                                                          |
|--------------|-------------------------------------------------------------------|
| Unit         | Mock `OAuthProvider` trait impls; test PKCE generation            |
| Unit         | Test `CodexProvider` and `AntigravityProvider` independently      |
| Unit         | Test `OAuthRegistry` with mock providers                          |
| Unit         | Test `TokenStorage` with temp directories                         |
| Unit         | Test Antigravity request body wrapping                            |
| Integration  | `wiremock`: mock `auth.openai.com` for Codex                     |
| Integration  | `wiremock`: mock `oauth2.googleapis.com` for Antigravity          |
| Config       | Snapshot tests for TOML with Codex / Antigravity / both / none   |
| Onboard      | Test `OnboardConfig` generation for OAuth vs API key paths        |
| E2E          | Manual: `clawshell onboard` → select Codex → proxy request       |
| E2E          | Manual: `clawshell onboard` → select Antigravity → proxy request |
| Existing     | All existing tests must pass (OAuth is opt-in)                    |

---

## 9. Implementation Phases

### Phase 1: OAuth Framework (Medium Effort)
1. Add `oauth2`, `open`, `chrono` dependencies to `Cargo.toml`.
2. Create `src/oauth/mod.rs` — `OAuthProvider` trait, `OAuthTokens`, `OAuthError`.
3. Create `src/oauth/storage.rs` — per-provider token persistence.
4. Create `OAuthRegistry` with provider registration, token management, refresh tasks.
5. Unit tests with mock providers.

### Phase 2: Codex Provider (Medium Effort)
1. Create `src/oauth/codex.rs` — browser PKCE flow + device code flow.
2. Implement `inject_auth()` (Bearer token).
3. Unit + integration tests with `wiremock`.

### Phase 3: Antigravity Provider (Medium Effort)
1. Create `src/oauth/antigravity.rs` — browser PKCE flow + headless fallback.
2. Implement `inject_auth()` (Bearer + Google-specific headers).
3. Implement `prepare_request_body()` (Gemini-style wrapping).
4. Implement `upstream_url()` (endpoint resolution).
5. Implement project ID discovery via `loadCodeAssist`.
6. Unit + integration tests.

### Phase 4: Config & Key Integration (Small Effort)
1. Add `[[oauth_providers]]` to `config.rs`.
2. Add `AuthMethod` enum and `oauth_provider` field to `KeyMapping`.
3. Extend `ResolvedKey` / `KeySource` in `keys.rs`.
4. Update `proxy.rs` — dispatch to `inject_auth()` + `prepare_request_body()` + 401-retry.
5. Wire `OAuthRegistry` into `AppState` in `lib.rs`.

### Phase 5: Onboarding Integration (Medium Effort)
1. Add "Codex / ChatGPT (OAuth)" and "Antigravity / Google (OAuth)" to provider menu
   in `onboard/interactive.rs`.
2. Add OAuth login flow branch (skip API key prompt, run browser/headless flow).
3. Update `OnboardConfig` with `AuthMethod` enum in `onboard/types.rs`.
4. Update `config_render.rs` to generate `[[oauth_providers]]` and `auth = "oauth"`.
5. Handle re-onboard detection (existing OAuth tokens).

### Phase 6: Documentation & Polish
1. Update README.
2. Update example config.
3. Document ToS considerations per provider.

---

## 10. Risks and Mitigations

| Risk                                           | Likelihood | Impact | Mitigation                                    |
|------------------------------------------------|------------|--------|-----------------------------------------------|
| OpenAI changes Codex client ID                 | Medium     | High   | Make configurable; monitor changes             |
| Google blocks Antigravity 3P usage             | High       | High   | Make configurable; document risk; feature flag |
| OAuth tokens rejected on standard endpoints    | Low        | High   | Verify during Phase 2/3; abort if incompatible |
| Antigravity API format changes                 | Medium     | Medium | Version-pin User-Agent; test against live API  |
| Rate limits differ for OAuth vs. API key       | Medium     | Medium | Document limitation; let users choose method   |
| Token refresh fails silently                   | Low        | Medium | Aggressive logging; prompt re-onboard          |
| Anthropic maintains Claude OAuth ban           | Very High  | Low    | Already accounted for — not implementing        |

---

## 11. Backward Compatibility

Fully opt-in.  No `[[oauth_providers]]` = identical behavior to today.  The existing
provider options (OpenAI, OpenRouter, Anthropic) in `clawshell onboard` work exactly
as before.  No new CLI subcommands — no change to the command interface.

---

## 12. Decisions Required

1. **Codex endpoint compatibility:** Verify ChatGPT OAuth tokens work on standard OpenAI API.
2. **Codex client ID policy:** Use Codex CLI's client ID or register our own?
3. **Antigravity client ID:** Use the known Antigravity client ID or register?
4. **Antigravity body transformation:** Should ClawShell translate OpenAI-format to
   Gemini-style, or require clients to send Gemini-format directly?
5. **ToS risk acceptance:** Proceed with documented risk, or defer Antigravity?
