# ClawShell 🛡️

![ClawShell Banner](docs/images/banner.png)

> **Powered by Runta. The essential safety harness for OpenClaw's PII & Sensitive Credentials.**

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![GitHub Actions Workflow Status](https://img.shields.io/github/actions/workflow/status/clawshell/clawshell/rust.yml)](https://github.com/clawshell/clawshell/actions)
[![NPM Version](https://img.shields.io/npm/v/%40clawshell%2Fclawshell)](https://www.npmjs.com/package/@clawshell/clawshell)
[![Crates.io Version](https://img.shields.io/crates/v/clawshell)](https://crates.io/crates/clawshell)


## 📖 Introduction

**ClawShell** is a security-privileged process for the **OpenClaw** ecosystem. It sits between OpenClaw and upstream LLM API providers (OpenAI, Anthropic), performing virtual-to-real API key mapping and DLP (Data Loss Prevention) scanning on request and response bodies.

OpenClaw never holds real API keys, only virtual keys that ClawShell swaps for real ones before forwarding requests upstream. Real keys are stored in a privileged config directory (`/etc/clawshell`) protected by Unix file system permissions.

## Key Features

### 1. API Token Secure Binding

ClawShell maps virtual API keys to real provider keys so that OpenClaw never has direct access to real credentials.

- **Key Isolation**: Real API keys are stored in `/etc/clawshell/clawshell.toml`, readable only by the `clawshell` system user. OpenClaw holds only virtual keys.
- **Multi-Provider Support**: Maps keys to OpenAI or Anthropic, injecting the correct authentication header format (`Authorization: Bearer` for OpenAI, `x-api-key` for Anthropic).

### 2. PII Safety Net (DLP)

ClawShell scans HTTP request and response bodies for sensitive data using configurable regex patterns.

- **Request Scanning**: Detects PII (SSNs, credit card numbers, emails, etc.) in outbound requests. Patterns can be configured to either block the request or redact the matched text before forwarding.
- **Response Scanning**: Optionally scans upstream responses and redacts detected PII before returning to OpenClaw. Streaming (SSE) responses are passed through without scanning.
- **Custom Patterns**: Define sensitive data patterns using regex in the TOML config, each with a `block` or `redact` action.

### 3. Seamless Integration

- **Drop-in Sidecar**: Deploys alongside OpenClaw without requiring re-install — the `clawshell onboard` command automatically configure OpenClaw to point at ClawShell's address and it forwards all requests upstream.
- **No External Dependencies**: Uses Unix file system permissions to protect secrets. No IdP, Vault, or external key management service required.

### 4. Ultra Lightweight and Scalable

- Runs in under 10MB of memory.
- Written in Rust with Tokio.

## Architecture

```
                               ║ security boundary (Unix File System Permissions)
                               ║
                               ║  ┌────────────────────┐
                               ║  │  /etc/clawshell    │
                               ║  │  ┄ real API keys   │
                               ║  │  ┄ DLP patterns    │
                               ║  └────────┬───────────┘
                               ║     reads │
                               ║  ┌────────┴───────────┐
  ┌──────────────┐  REQUEST    ║  │                    │   REQUEST       ┌────────────┐
  │              ├──(virtual───╫─►│    ClawShell       ├──-(real key,───►│            │
  │   OpenClaw   │   key)      ║  │                    │   PII redacted) │   OpenAI   │
  │              │             ║  │  DLP scan          │                 │     or     │
  │ holds only   │  RESPONSE   ║  │  real-key mapping  │   RESPONSE      │  Anthropic │
  │ virtual keys │◄────────────║◄─┤                    │◄────────────────┤            │
  │              │             ║  │                    │                 │            │
  └──────────────┘             ║  └────────────────────┘                 └────────────┘
                               ║
```

OpenClaw only holds virtual keys and cannot access the real API keys stored in the privileged config.
ClawShell swaps virtual keys for real ones and scans for PII before forwarding requests upstream.

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
cargo build --release
ls -al target/release/clawshell
```

#### Cross-compile on Linux/arm64

```bash
wget https://musl.cc/x86_64-linux-musl-cross.tgz -O /tmp/musl-cross.tgz
tar -xzf /tmp/musl-cross.tgz -C /tmp
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="/tmp/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc" \
cargo build --release --target x86_64-unknown-linux-musl
```


## Advanced Usage

### Onboarding

The `onboard` command is an interactive setup wizard that must be run with `sudo`. It:

1. Creates the `clawshell` system user.
2. Creates and secures `/etc/clawshell` (mode 700) and `/var/log/clawshell`.
3. Walks you through provider selection, API key entry, and virtual key generation.
4. Writes the ClawShell config to `/etc/clawshell/clawshell.toml`.
5. Updates your OpenClaw configuration to route through ClawShell.
6. Starts the ClawShell daemon.

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

### Customized Configuration

ClawShell reads its config from `/etc/clawshell/clawshell.toml`. You can view or edit it with:

```bash
sudo clawshell config          # print current config
sudo clawshell config --edit   # open in $EDITOR
```

A minimal config looks like this:

```toml
version = "0.0.2"
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
    { name = "amex_card", regex = '\b3[47][0-9]{13}\b',              action = "redact" },
]
```

If `start`, `restart`, `stop`, `config --edit`, `onboard`, or `uninstall` reports that migration is required, run:

```bash
sudo clawshell migrate-config --config /etc/clawshell/clawshell.toml
```

See [`clawshell.example.toml`](clawshell.example.toml) for a full example.

### Uninstall

```bash
sudo clawshell uninstall
```

## License

This project is licensed under the [Apache License 2.0](LICENSE).
