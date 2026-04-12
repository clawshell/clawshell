# ClawShell 🛡️

![ClawShell Banner](docs/images/banner.png)

> **Powered by Runta. The essential safety harness for OpenClaw/Hermes Agent's PII & Sensitive Credentials.**

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![GitHub Actions Workflow Status](https://img.shields.io/github/actions/workflow/status/clawshell/clawshell/rust.yml)](https://github.com/clawshell/clawshell/actions)
[![NPM Version](https://img.shields.io/npm/v/%40clawshell%2Fclawshell)](https://www.npmjs.com/package/@clawshell/clawshell)
[![Crates.io Version](https://img.shields.io/crates/v/clawshell)](https://crates.io/crates/clawshell)


## 📖 Introduction

**ClawShell** is a security-privileged process for the **OpenClaw/Hermes Agent** ecosystem. It sits between OpenClaw/Hermes Agent and upstream LLM API providers (OpenAI, Anthropic, OpenRouter), performing virtual-to-real API key mapping and DLP (Data Loss Prevention) scanning on request and response bodies. It can also expose an Email read endpoint with sender allowlist/denylist filtering.

OpenClaw/Hermes Agent never holds real API keys, only virtual keys that ClawShell swaps for real ones before forwarding requests upstream. Real keys are stored in a privileged config directory (`/etc/clawshell`) protected by Unix file system permissions.

## Key Features

### 1. API Token Secure Binding

ClawShell maps virtual API keys to real provider keys so that OpenClaw/Hermes Agent never has direct access to real credentials.

- **Key Isolation**: Real API keys are stored in `/etc/clawshell/clawshell.toml`, readable only by the `clawshell` system user. OpenClaw/Hermes Agent holds only virtual keys.
- **Multi-Provider Support**: Maps keys to OpenAI or Anthropic, injecting the correct authentication header format (`Authorization: Bearer` for OpenAI, `x-api-key` for Anthropic).

### 2. PII Safety Net (DLP)

ClawShell scans HTTP request and response bodies for sensitive data using configurable regex patterns.

- **Request Scanning**: Detects PII (SSNs, credit card numbers, emails, etc.) in outbound requests. Patterns can be configured to either block the request or redact the matched text before forwarding.
- **Response Scanning**: Optionally scans upstream responses and redacts detected PII before returning to OpenClaw/Hermes Agent. Streaming (SSE) responses are passed through without scanning.
- **Custom Patterns**: Define sensitive data patterns using regex in the TOML config, each with a `block` or `redact` action.

### 3. Sensitive Email Isolation

ClawShell supports sender-based email filtering so each virtual key only sees mailbox content based on sender rules.

- **Sender Filtering**: Filter emails by sender.
- **Key Isolation**: IMAP credentials are stored in `/etc/clawshell/clawshell.toml`, readable only by the `clawshell` system user. OpenClaw/Hermes Agent holds only virtual keys.
- **Provider Support**: Built-in Gmail and Outlook presets, with manual IMAP setup for other providers.

### 4. OAuth Authentication (Codex / ChatGPT)

ClawShell supports OAuth-based authentication as an alternative to static API keys.

- **Device Code Flow**: Log in via a one-time code — no browser required on the server. The onboard wizard prints a URL and code; authorize from any device.
- **Automatic Token Refresh**: Access tokens are refreshed transparently before they expire.
- **Request Translation**: Automatically translates OpenAI Chat Completions API requests to the ChatGPT Responses API format when using Codex OAuth.

### 5. Runtime Statistics

ClawShell exposes running counters at `GET /admin/stats` so operators can audit proxy activity since startup and across restarts.

- **What's Counted**: Total requests protected, total upstream `prompt_tokens` / `completion_tokens` / `total_tokens` (parsed from non-streaming JSON responses — SSE streams are not counted), number of emails hidden by the sender policy, and a per-address breakdown of filtered senders.
- **Loopback-Only**: The endpoint is reachable without a virtual key but only from `127.0.0.1` / `::1` peers; non-loopback clients receive `403`.
- **Persistent**: Counters are flushed to disk every 30 seconds and on graceful shutdown. The location is a required config field — set `[stats] persist_path = "..."` in `clawshell.toml` (typically `/var/lib/clawshell/stats.json` under the hardened systemd unit, since `/etc/clawshell` is read-only there).
- **Bounded**: The filtered-address map is capped at 10,000 unique entries; further unique addresses are aggregated under an `<overflow>` bucket so memory stays bounded.

### 6. Seamless Integration

- **Drop-in Sidecar**: The `clawshell onboard` wizard configures exactly one downstream LLM client per run — either OpenClaw or [Hermes Agent](https://github.com/NousResearch/hermes-agent) — to route all requests through ClawShell's proxy. See [Agent Target (pick one)](#agent-target-pick-one).
- **No External Dependencies**: Uses Unix file system permissions to protect secrets. No IdP, Vault, or external key management service required.

### 7. Ultra Lightweight and Scalable

- Runs in under 10MB of memory.
- Written in Rust with Tokio.

## Architecture

```
                               ║ security boundary (Unix File System Permissions)
                               ║
                               ║  ┌───────────────────────────┐
                               ║  │  /etc/clawshell           │
                               ║  │  ┄ real API keys          │
                               ║  │  ┄ DLP patterns           │
                               ║  │  ┄ email sender rules     │
                               ║  │  ┄ IMAP account creds     │
                               ║  └──────────┬────────────────┘
                               ║       reads │
                               ║  ┌──────────┴────────────────┐
  ┌──────────────┐  REQUEST    ║  │                           │   REQUEST       ┌────────────┐
  │              ├──(virtual───╫─►│       ClawShell           ├──-(real key,───►│            │
  │  OpenClaw or │   key)      ║  │                           │   PII redacted) │  OpenAI /  │
  │ Hermes Agent │             ║  │  DLP scan                 │                 │ Anthropic/ │
  │ holds only   │  RESPONSE   ║  │  real-key mapping         │   RESPONSE      │ OpenRouter │
  │ virtual keys │◄────────────║◄─┤  email sender filtering   │◄────────────────┤            │
  │              │             ║  │                           │                 └────────────┘
  │              │  EMAIL GET  ║  │                           │   IMAP fetch    ┌────────────┐
  │              ├───(virtual──║  |                           |───(real key)───►|            |
  │              │    key)     ║  │                           │                 │ IMAP       │
  │              │             ║  │                           │                 | Provider   │
  │              │             ║  │                           │    RESPONSE     │ Gmail /    │
  │              │  filtered   ║  │                           │◄────────────────│ Outlook /  │
  │              │◄────────────║◄─|                           |                 │ custom     │
  └──────────────┘             ║  |                           |                 └────────────┘
                               ║  └───────────────────────────┘
```

OpenClaw/Hermes Agent only holds virtual keys and cannot access the real API keys stored in the privileged config.

ClawShell swaps virtual keys for real ones and scans for PII before forwarding requests upstream.

ClawShell also enforces sender-based filtering before returning email data.

## Installation

### Cargo

```bash
cargo install clawshell --locked

# Requires privilege to set up the security boundary
sudo clawshell onboard
```

### NPM

```bash
npm install -g @clawshell/clawshell

# Requires privilege to set up the security boundary
sudo clawshell onboard
```

### Build from Source

```bash
RUSTFLAGS="--remap-path-prefix=$(pwd)=. --remap-path-prefix=$HOME=/" cargo build --release
ls -al target/release/clawshell
```

#### Cross-compile on Linux/arm64

```bash
wget https://musl.cc/x86_64-linux-musl-cross.tgz -O /tmp/musl-cross.tgz
tar -xzf /tmp/musl-cross.tgz -C /tmp
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="/tmp/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc" \
RUSTFLAGS="--remap-path-prefix=$(pwd)=. --remap-path-prefix=$HOME=/" \
cargo build --release --target x86_64-unknown-linux-musl
```


## Advanced Usage

### Onboarding

The `onboard` command is an interactive setup wizard that must be run with `sudo`. It:

1. Asks which downstream agent to wire through ClawShell — **OpenClaw** or **Hermes Agent** (exactly one per run).
2. Creates the `clawshell` system user.
3. Creates and secures `/etc/clawshell` (mode 700) and `/var/log/clawshell`.
4. Walks you through provider selection, API key entry, and virtual key generation.
5. Writes the ClawShell config to `/etc/clawshell/clawshell.toml`.
6. Wires the chosen agent through ClawShell (patches `~/.openclaw/openclaw.json` for OpenClaw, or runs `hermes config set` for Hermes).
7. Starts the ClawShell daemon.

```bash
sudo clawshell onboard
```

### More Commands

```bash
# Start (daemonizes by default)
sudo clawshell start

# Start in the foreground
sudo clawshell start --foreground

# Start with a custom config file
sudo clawshell start -c /path/to/clawshell.toml

# Check status
clawshell status

# View logs
clawshell logs
clawshell logs --level error
clawshell logs --follow

# Restart / Stop
sudo clawshell restart
sudo clawshell stop

# Migrate config schema to current version
sudo clawshell migrate-config
```

By default ClawShell listens on `127.0.0.1:18790`.

You can override the bind address at runtime with environment variables:

```bash
CLAWSHELL_SERVER_HOST=0.0.0.0 CLAWSHELL_SERVER_PORT=17890 clawshell start --foreground
```

### Customized Configuration

ClawShell reads its config from `/etc/clawshell/clawshell.toml`. You can view or edit it with:

```bash
sudo clawshell config          # print current config
sudo clawshell config --edit   # open in $EDITOR
```

A minimal config looks like this:

```toml
version = "0.2.1"
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
openai_base_url = "https://api.openai.com"
anthropic_base_url = "https://api.anthropic.com"

# Virtual-to-real API key mappings
[[keys]]
virtual_key = "vk-alice-001"
real_key = "sk-your-real-openai-key-here"
provider = "openai"

[[keys]]
virtual_key = "vk-claude-001"
real_key = "sk-ant-your-real-anthropic-key-here"
provider = "anthropic"

# Data Loss Prevention (DLP)
# action = "block"  -> reject the request with 400
# action = "redact" -> replace matches with [REDACTED:<name>] and forward
[dlp]
scan_responses = false
patterns = [
    { name = "ssn",       regex = '\b\d{3}-\d{2}-\d{4}\b',          action = "redact" },
    { name = "visa_card", regex = '\b4[0-9]{12}(?:[0-9]{3})?\b',    action = "redact" },
    { name = "amex_card", regex = '\b3[47][0-9]{13}\b',             action = "redact" },
]

# Email secure endpoint
[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com", "@trusted.org"]
deny_senders = []
default_max_results = 50

[[email.accounts]]
virtual_key = "vk-email-001"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
imap_host = "imap.gmail.com"
imap_port = 993
# Outlook preset example:
# imap_host = "imap-mail.outlook.com"
```

### OAuth Authentication (Codex / ChatGPT)

Instead of a static API key, you can authenticate via OAuth using your ChatGPT / Codex account.

#### Setup via onboard

During `sudo clawshell onboard`, select **"Codex / ChatGPT (OAuth)"** as the provider. The wizard will start a device code flow:

1. A URL and one-time code are printed to the terminal.
2. Open the URL on any device and enter the code.
3. Once authorized, tokens are saved automatically to `/etc/clawshell/oauth/`.

No browser is required on the server.

#### Manual configuration

To configure OAuth manually in `clawshell.toml`:

```toml
# OAuth-backed key — no real_key needed
[[keys]]
virtual_key = "vk-codex-001"
auth = "oauth"
oauth_provider = "codex"
provider = "openai"

# OAuth provider definition
[[oauth_providers]]
provider = "codex"
# Optional overrides (defaults work for ChatGPT):
# client_id = "app_EMoamEEZ73f0CkXaXp7hrann"
# auth_url = "https://auth.openai.com/authorize"
# token_url = "https://auth.openai.com/oauth/token"
```

ClawShell handles token refresh automatically. When using Codex OAuth, requests to `/v1/chat/completions` are translated to the ChatGPT Responses API format and routed to `chatgpt.com/backend-api/codex`.

If `start`, `restart`, `stop`, `config --edit`, `onboard`, or `uninstall` reports that migration is required, run:

```bash
sudo clawshell migrate-config --config /etc/clawshell/clawshell.toml
```

See [`clawshell.example.toml`](clawshell.example.toml) for a full example.

### Agent Target (pick one)

`sudo clawshell onboard` begins with a single, mandatory choice:

```
=== Agent Target ===
? Which downstream agent should ClawShell wire through?
  > OpenClaw
    Hermes Agent
```

Each onboard run configures **exactly one** downstream client. There's no "also configure the other one" path — switching later means re-running `sudo clawshell onboard` and picking the other target. The prompt has no default preselection, so you pick explicitly every time.

#### OpenClaw target

When you pick OpenClaw, the wizard:

- Backs up `~/.openclaw/openclaw.json` (numbered `.bak` files, mode 000).
- Shells out to `openclaw config set` to patch three paths: `env.CLAWSHELL_API_KEY`, `agents.defaults.models.clawshell/<model>`, and `models.providers.clawshell`.
- Writes a `get-email-messages` skill bundle to `<openclaw_root>/skills/` when email integration is enabled.
- Offers to run `openclaw models set clawshell` and `openclaw gateway restart` at the end.

This is the historical onboarding flow and is unchanged by the target-selection rework.

#### Hermes Agent target

When you pick [Hermes Agent](https://github.com/NousResearch/hermes-agent), the wizard:

- Skips every OpenClaw step — `~/.openclaw/` is **not** touched.
- Writes a `get-email-messages` skill bundle to `~/.hermes/skills/` (owned by your invoking user, not root) when email integration is enabled. Hermes auto-discovers skills from that directory.
- Shells out to `hermes config set` to write:

  | Key | Value |
  |---|---|
  | `model.provider` | `custom` |
  | `model.base_url` | `http://<server_host>:<server_port>/v1` |
  | `model.default`  | the model ID you chose during onboard |
  | `model.api_key`  | your ClawShell **virtual** key (never the real upstream key) |

The `hermes` binary must be on your `PATH`. ClawShell drops root privileges before invoking it so writes land under your normal user account, not root's.

#### Manual Hermes configuration

If you'd rather skip the wizard's Hermes integration, run the equivalent commands from your user account (not root):

```bash
hermes config set model.provider custom
hermes config set model.base_url http://127.0.0.1:18790/v1
hermes config set model.default  <your-model-id>
hermes config set model.api_key  <your-clawshell-virtual-key>
```

Then verify with `hermes config show`.

#### Reverting Hermes

Hermes has no `config unset` subcommand. To detach Hermes from ClawShell, set the provider back to auto-detect and Hermes will pick another upstream based on the credentials it still has:

```bash
hermes config set model.provider auto
```

### Uninstall

```bash
sudo clawshell uninstall
```

## License

This project is licensed under the [Apache License 2.0](LICENSE).
