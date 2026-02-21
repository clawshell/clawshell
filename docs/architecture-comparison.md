# Architecture Comparison: Current vs. Multi-Provider OAuth

This document compares ClawShell's **current architecture** (static API keys only)
with the **proposed architecture** after adding multi-provider OAuth support.

The first version (v1) implements two providers: **Codex (OpenAI)** and
**Antigravity (Google)**.  OAuth providers are integrated into the existing
`clawshell onboard` wizard — no new CLI subcommands are added.

---

## 1. High-Level Flow

### Current (Static API Key)

```
┌──────────┐    Authorization: Bearer vk-001    ┌─────────────┐   Authorization: Bearer sk-real-...   ┌──────────────┐
│          │ ─────────────────────────────────►  │             │ ──────────────────────────────────►  │              │
│ OpenClaw │                                    │  ClawShell  │                                     │  OpenAI API  │
│          │ ◄─────────────────────────────────  │             │ ◄──────────────────────────────────  │              │
└──────────┘          response                  └─────────────┘           response                  └──────────────┘
                                                      │
                                                 Lookup vk-001
                                                 in BTreeMap
                                                      │
                                                      ▼
                                              clawshell.toml
                                              (static real_key)
```

**Characteristics:**
- One-time setup via `clawshell onboard`: paste API key
- Key never changes — no refresh needed
- Key lives on disk permanently in plaintext (protected by Unix file permissions)
- No external auth server interaction at runtime

### Proposed (Multi-Provider OAuth)

```
              clawshell onboard (same command, expanded menu)
              ┌──────────────────────────────────────────────────┐
              │                                                  │
              │  Select a model provider:                        │
              │    1. OpenAI              → prompt for API key   │
              │    2. OpenRouter          → prompt for API key   │
              │    3. Anthropic           → prompt for API key   │
              │    4. Codex / ChatGPT     → OAuth browser flow   │  ← NEW
              │    5. Antigravity / Google → OAuth browser flow   │  ← NEW
              │                                                  │
              └──────────────────────────────────────────────────┘

                         RUNTIME (per request)
┌──────────┐    Bearer vk-001    ┌──────────────────────────────────────────────────┐
│          │ ──────────────────► │               ClawShell                           │
│ OpenClaw │                    │                                                   │
│          │ ◄────────────────── │  1. Lookup vk-001 → ResolvedKey { source, prov }  │
└──────────┘     response       │  2. KeySource?                                    │
                                │     ├── Static(key) → inject key (existing logic) │
                                │     └── OAuth{provider_id}                        │
                                │          ├── registry.inject_auth(id, headers)     │
                                │          ├── registry.prepare_request_body(id, b)  │
                                │          └── registry.upstream_url(id)             │
                                │  3. Forward to upstream                            │
                                │  4. On 401 (OAuth only) → refresh + retry         │
                                └──────────────────────────────────────────────────┘

                         BACKGROUND (one task per active provider)
                ┌─────────────────────────────────────────────────────────────┐
                │  codex:        sleep(75% of ~8-day TTL) → auth.openai.com  │
                │  antigravity:  check 60s before expiry  → googleapis.com   │
                └─────────────────────────────────────────────────────────────┘
```

---

## 2. Module Comparison

### Current Module Map

```
src/
├── main.rs               CLI dispatch, daemon lifecycle
├── lib.rs                AppState, build_router(), handle_request()
├── cli.rs                Clap CLI definitions
├── config.rs             TOML config model + validation
├── keys.rs               KeyManager: virtual→real key BTreeMap lookup
├── dlp.rs                DLP regex scanner (block/redact)
├── proxy.rs              ProxyClient: upstream HTTP forwarding
├── onboard/
│   ├── mod.rs            Public API
│   ├── interactive.rs    TUI wizard prompts
│   ├── types.rs          OnboardConfig struct
│   ├── config_render.rs  TOML generation
│   ├── credentials.rs    API key detection
│   └── ...               backup, openclaw_json, skills, etc.
├── process.rs            PID file, privilege drop
├── tui.rs                Terminal UI
└── platform/
    ├── mod.rs            Platform dispatch
    ├── linux.rs          systemd, useradd
    └── macos.rs          launchctl, dscl
```

### Proposed Module Map

```
src/
├── main.rs               CLI dispatch + OAuthRegistry init + refresh tasks  ← MODIFIED
├── lib.rs                AppState + OAuthRegistry                           ← MODIFIED
├── cli.rs                Clap CLI (UNCHANGED — no new subcommands)          ← UNCHANGED
├── config.rs             TOML config + [[oauth_providers]] + auth field     ← MODIFIED
├── keys.rs               KeyManager: virtual→KeySource (Static | OAuth{id}) ← MODIFIED
├── dlp.rs                DLP regex scanner (block/redact)                   ← UNCHANGED
├── oauth/                                                                   ← NEW DIR
│   ├── mod.rs            OAuthProvider trait, OAuthRegistry, OAuthTokens    ← NEW
│   ├── codex.rs          Codex (OpenAI): PKCE + device code                ← NEW [v1]
│   ├── antigravity.rs    Antigravity (Google): PKCE + headless URL          ← NEW [v1]
│   └── storage.rs        Per-provider token persistence                     ← NEW
├── proxy.rs              ProxyClient + inject_auth + prepare_body + 401     ← MODIFIED
├── onboard/
│   ├── mod.rs            Public API                                         ← UNCHANGED
│   ├── interactive.rs    Provider menu + OAuth login branch                 ← MODIFIED
│   ├── types.rs          OnboardConfig with AuthMethod enum                 ← MODIFIED
│   ├── config_render.rs  TOML gen + [[oauth_providers]] rendering           ← MODIFIED
│   ├── credentials.rs    API key detection                                  ← UNCHANGED
│   └── ...               backup, openclaw_json, skills, etc.                ← UNCHANGED
├── process.rs            PID file, privilege drop                           ← UNCHANGED
├── tui.rs                Terminal UI                                        ← UNCHANGED
└── platform/                                                                ← UNCHANGED
```

**Summary: 1 new directory with 4 files, 7 modified files, rest unchanged.**

---

## 3. Core Abstraction: `OAuthProvider` Trait

### How Providers Differ

| Trait Method              | Codex (OpenAI)                              | Antigravity (Google)                           |
|---------------------------|---------------------------------------------|------------------------------------------------|
| `id()`                    | `"codex"`                                   | `"antigravity"`                                |
| `display_name()`          | `"Codex (OpenAI)"`                          | `"Antigravity (Google)"`                       |
| `supports_device_code()`  | `true`                                      | `false`                                        |
| `supports_headless_url()` | `false`                                     | `true`                                         |
| `login_browser()`         | PKCE → `auth.openai.com`                    | PKCE → `accounts.google.com` + project discovery|
| `login_headless()`        | Device code polling                         | Print URL, paste redirect back                 |
| `refresh()`               | POST `auth.openai.com/oauth/token`          | POST `oauth2.googleapis.com/token`             |
| `inject_auth()`           | `Authorization: Bearer <token>`             | `Authorization: Bearer` + `X-Goog-Api-Client` + `Client-Metadata` |
| `prepare_request_body()`  | `None` (pass-through)                       | `Some(wrapped)` (Gemini-style envelope)        |
| `upstream_url()`          | `None` (use `[upstream].base_url`)          | `Some("cloudcode-pa.googleapis.com/...")`      |

---

## 4. Data Structures Comparison

### `ResolvedKey`

**Current:**

```rust
pub struct ResolvedKey {
    pub real_key: String,
    pub provider: Provider,
}
```

**Proposed:**

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

### `AppState`

**Current:**

```rust
pub struct AppState {
    pub key_manager: Arc<KeyManager>,
    pub dlp_scanner: Arc<DlpScanner>,
    pub proxy_client: Arc<ProxyClient>,
}
```

**Proposed:**

```rust
pub struct AppState {
    pub key_manager: Arc<KeyManager>,
    pub dlp_scanner: Arc<DlpScanner>,
    pub proxy_client: Arc<ProxyClient>,
    pub oauth_registry: Option<Arc<OAuthRegistry>>,
}
```

### `OnboardConfig`

**Current:**

```rust
pub struct OnboardConfig {
    pub provider: String,
    pub model: String,
    pub real_api_key: String,          // always required
    pub virtual_api_key: String,
    pub openclaw_config_path: PathBuf,
    pub server_host: String,
    pub server_port: u16,
    pub email: Option<OnboardEmailConfig>,
}
```

**Proposed:**

```rust
pub enum AuthMethod {
    ApiKey { real_api_key: String },
    OAuth { provider_id: String },      // tokens stored during onboard flow
}

pub struct OnboardConfig {
    pub provider: String,
    pub model: String,
    pub auth: AuthMethod,               // was: pub real_api_key: String
    pub virtual_api_key: String,
    pub openclaw_config_path: PathBuf,
    pub server_host: String,
    pub server_port: u16,
    pub email: Option<OnboardEmailConfig>,
}
```

### `Config` / `KeyMapping`

**Current:**

```rust
pub struct KeyMapping {
    pub virtual_key: String,
    pub real_key: String,
    pub provider: Provider,
}
```

**Proposed:**

```rust
pub struct KeyMapping {
    pub virtual_key: String,
    pub real_key: Option<String>,       // optional when auth = "oauth"
    pub provider: Provider,
    pub auth: AuthMethod,               // defaults to Static
    pub oauth_provider: Option<String>, // "codex" or "antigravity"
}
```

---

## 5. Configuration Comparison

### Current `clawshell.toml`

```toml
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://api.openai.com"

[[keys]]
virtual_key = "vk-alice-001"
real_key = "sk-abc123..."
provider = "openai"

[dlp]
scan_responses = true
patterns = [
  { name = "ssn", regex = '\b\d{3}-\d{2}-\d{4}\b', action = "redact" },
]
```

### Proposed (Generated by `clawshell onboard` When OAuth Is Selected)

```toml
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://api.openai.com"

# OAuth-backed key — generated by onboard wizard
[[keys]]
virtual_key = "vk-chatgpt-001"
provider = "openai"
auth = "oauth"
oauth_provider = "codex"

[[oauth_providers]]
provider = "codex"

[dlp]
scan_responses = true
patterns = [
  { name = "ssn", regex = '\b\d{3}-\d{2}-\d{4}\b', action = "redact" },
]
```

**When selecting API key providers, the output is identical to today.**

---

## 6. Onboarding Flow Comparison

This is the primary user-facing change — OAuth is integrated into the existing
`clawshell onboard` command, not a separate subcommand.

### Current Onboarding Flow

```
$ sudo clawshell onboard

  Select a model provider:
    ► OpenAI
      OpenRouter
      Anthropic

  Enter the model name: [gpt-5.2-chat-latest]
  Enter the real API key: ****************************
  Enter the virtual API key: [{clawshell-virtual-key-openai}]
  ... (email, OpenClaw config, server settings)
```

### Proposed Onboarding Flow

```
$ sudo clawshell onboard

  Select a model provider:
    ► OpenAI                           ← existing (API key)
      OpenRouter                       ← existing (API key)
      Anthropic                        ← existing (API key)
      Codex / ChatGPT (OAuth)          ← NEW
      Antigravity / Google (OAuth)     ← NEW

  ─── If user selects "Codex / ChatGPT (OAuth)" ──────────

  Enter the model name: [gpt-5.2-chat-latest]

  Opening browser for ChatGPT login...
  (browser opens to auth.openai.com)
  ✓ Login successful. Tokens saved.

  Enter the virtual API key: [{clawshell-virtual-key-codex}]
  ... (email, OpenClaw config, server settings — unchanged)

  ─── If user selects "Antigravity / Google (OAuth)" ─────

  Enter the model name: [gemini-3-pro]

  Opening browser for Google login...
  (browser opens to accounts.google.com)
  ✓ Login successful. Project ID: proj-abc-123. Tokens saved.

  Enter the virtual API key: [{clawshell-virtual-key-antigravity}]
  ... (email, OpenClaw config, server settings — unchanged)

  ─── If user selects "OpenAI" / "OpenRouter" / "Anthropic" ──

  (Identical to today — prompt for API key)
```

**In headless (SSH) environments:**

```
  Codex:       "Enter the device code shown in your browser: ___"
  Antigravity: "Visit this URL, then paste the redirect URL here: ___"
```

---

## 7. CLI Commands Comparison

### Current

```
clawshell start       Start the proxy daemon
clawshell stop        Stop the daemon
clawshell status      Check daemon status
clawshell restart     Restart the daemon
clawshell logs        View/tail log file
clawshell config      Display/edit config
clawshell onboard     Interactive setup wizard
clawshell uninstall   Remove ClawShell
clawshell version     Print version
```

### Proposed

```
clawshell start       Start daemon (+ spawn refresh tasks if OAuth configured)  ← MODIFIED behavior
clawshell stop        Stop the daemon                                           ← UNCHANGED
clawshell status      Check daemon status                                       ← UNCHANGED
clawshell restart     Restart the daemon                                        ← UNCHANGED
clawshell logs        View/tail log file                                        ← UNCHANGED
clawshell config      Display/edit config                                       ← UNCHANGED
clawshell onboard     Setup wizard (now with OAuth provider options)             ← MODIFIED behavior
clawshell uninstall   Remove ClawShell (+ remove OAuth token files)             ← MODIFIED behavior
clawshell version     Print version                                             ← UNCHANGED
```

**No new subcommands.** The CLI interface is identical. Only the behavior of
`onboard`, `start`, and `uninstall` changes.

---

## 8. Request Pipeline Comparison

### Current: 5-Step Pipeline

```
Step 1  Extract Authorization header → extract_virtual_key()
Step 2  Resolve virtual key          → resolve() → ResolvedKey { real_key, provider }
Step 3  Buffer request body
Step 4  DLP scan request body
Step 5  Forward to upstream           → forward(real_key, provider)
Step 6  Optional DLP scan response
```

### Proposed: Pipeline With Provider-Aware Branching

```
Step 1  Extract Authorization header  → extract_virtual_key()
Step 2  Resolve virtual key           → resolve() → ResolvedKey { source, provider }

  ┌─── Static path (unchanged) ─────────────────────────────────────────────┐
  │ Step 3  Buffer body → DLP scan → Forward with static key                │
  └─────────────────────────────────────────────────────────────────────────┘

  ┌─── OAuth path (NEW) ───────────────────────────────────────────────────┐
  │ Step 3  Get access token via OAuthRegistry                             │
  │ Step 4  Buffer body → DLP scan                                         │
  │ Step 5  Provider-specific prep:                                        │
  │           Codex:       inject_auth (Bearer) + pass-through body        │
  │           Antigravity: inject_auth (Bearer + headers) + wrap body      │
  │ Step 6  Forward to provider-resolved upstream                          │
  │ Step 7  On 401: refresh token → retry once                             │
  │ Step 8  Optional DLP scan response                                     │
  └─────────────────────────────────────────────────────────────────────────┘
```

---

## 9. Upstream Request Comparison

### Codex (OpenAI) — Thin Provider

```
After ClawShell:
  POST /v1/chat/completions HTTP/1.1          ← same path as static key
  Authorization: Bearer eyJ...access_token     ← OAuth token
  Content-Type: application/json

  {"model":"gpt-4o","messages":[...]}          ← same body (pass-through)

Upstream: api.openai.com
```

### Antigravity (Google) — Thick Provider

```
After ClawShell:
  POST /v1internal:streamGenerateContent?alt=sse HTTP/1.1    ← different path
  Authorization: Bearer ya29...access_token
  X-Goog-Api-Client: google-cloud-sdk vscode_cloudshelleditor/0.1
  Client-Metadata: {"ideType":"ANTIGRAVITY",...}
  Content-Type: application/json

  { "project": "proj-abc-123", "model": "gemini-3-pro",     ← wrapped body
    "request": { "contents": [...] } }

Upstream: cloudcode-pa.googleapis.com
```

---

## 10. Credential Lifecycle Comparison

### Current: Static Key

```
    clawshell onboard            Runtime                   clawshell uninstall
┌───────────────────┐    ┌──────────────────────┐    ┌───────────────────┐
│ Paste API key     │    │ Key loaded at startup │    │ Deletes config    │
│ → clawshell.toml  │    │ Never changes         │    │ Key is gone       │
│                   │    │ No background tasks   │    │                   │
└───────────────────┘    └──────────────────────┘    └───────────────────┘
```

### Proposed: OAuth Token (Per Provider)

```
    clawshell onboard             Runtime                      clawshell uninstall
┌─────────────────────┐    ┌──────────────────────────────┐    ┌──────────────────┐
│ Select Codex →      │    │ Per-provider refresh tasks:   │    │ Deletes config + │
│ browser opens →     │    │  codex: sleep(75% of ~8d TTL)│    │ oauth/ directory  │
│ tokens saved to     │    │  antigravity: check 60s early│    │                  │
│ oauth/codex.json    │    │                              │    │ Tokens are gone   │
│                     │    │ On 401: refresh + retry       │    │                  │
│ OR                  │    │                              │    │ Re-onboard to     │
│                     │    │ Providers are independent     │    │ login again       │
│ Select Antigravity →│    │                              │    │                  │
│ browser opens →     │    │                              │    │                  │
│ project ID found →  │    │                              │    │                  │
│ oauth/antigravity.  │    │                              │    │                  │
│ json                │    │                              │    │                  │
└─────────────────────┘    └──────────────────────────────┘    └──────────────────┘
```

---

## 11. Security Model Comparison

### Current

```
/etc/clawshell/clawshell.toml  (0600)
  static API keys — permanent, manual revocation only
```

### Proposed (Additions)

```
/etc/clawshell/oauth/  (0700)
├── codex.json  (0600)        — ~8-day access token, single-use refresh
└── antigravity.json  (0600)  — ~1-hour access token, standard refresh

Improvements: short-lived tokens, auto-rotation, revocable from provider dashboard
New surface: token files on disk (same 0600 mitigation), ephemeral callback servers,
             network dependency on auth servers for refresh
```

---

## 12. Daemon Lifecycle Comparison

### Current Startup

```
main()
  ├── load config
  ├── build AppState { KeyManager, DlpScanner, ProxyClient }
  ├── bind socket → drop privileges → write PID
  └── serve (no background tasks)
```

### Proposed Startup

```
main()
  ├── load config
  ├── if [[oauth_providers]]:                    ← NEW
  │     ├── instantiate providers
  │     ├── load tokens from oauth/<id>.json
  │     └── create OAuthRegistry
  ├── build AppState { ..., OAuthRegistry? }
  ├── bind socket → drop privileges → write PID
  ├── if OAuthRegistry:                          ← NEW
  │     └── spawn per-provider refresh tasks
  └── serve
```

---

## 13. File Layout Comparison

### Current

```
/etc/clawshell/
├── clawshell.toml           config (0600)
└── config.json              onboarding metadata (0600)
```

### Proposed

```
/etc/clawshell/
├── clawshell.toml           config (0600)
├── config.json              onboarding metadata (0600)
└── oauth/                   token directory (0700)         ← NEW
    ├── codex.json           OpenAI OAuth tokens (0600)     ← NEW [v1]
    └── antigravity.json     Google OAuth tokens (0600)     ← NEW [v1]
```

---

## 14. Summary Table

| Aspect                | Current                         | Proposed                                          |
|-----------------------|---------------------------------|---------------------------------------------------|
| Auth methods          | Static API keys only            | Static + Codex OAuth + Antigravity OAuth          |
| CLI commands          | 9 commands                      | Same 9 commands (no new subcommands)              |
| Onboard menu          | OpenAI, OpenRouter, Anthropic   | + Codex/ChatGPT, Antigravity/Google               |
| Credential lifetime   | Permanent                       | Static: permanent; Codex: ~8d; Antigravity: ~1hr  |
| Background tasks      | None                            | One refresh task per active OAuth provider         |
| External auth calls   | None                            | HTTPS to provider auth servers (refresh only)      |
| New files on disk     | —                               | `oauth/codex.json`, `oauth/antigravity.json`       |
| New Rust modules      | —                               | `oauth/` (mod, codex, antigravity, storage)        |
| Modified modules      | —                               | 7 files (main, lib, config, keys, proxy, onboard×2)|
| New dependencies      | —                               | `oauth2`, `open`, `chrono`, `async-trait`          |
| Config format         | No OAuth section                | `[[oauth_providers]]` + `[[keys]].auth`            |
| Backward compatible   | —                               | Yes — no OAuth config = identical behavior         |
