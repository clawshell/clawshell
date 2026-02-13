# ClawShell 🛡️

![ClawShell Banner](docs/images/banner.png)

> **Powered by Runta. The essential safety harness for OpenClaw's PII & API data.**

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/clawshell/clawshell)
[![Version](https://img.shields.io/badge/version-0.0.1-orange.svg)]()

## 📖 Introduction

**ClawShell** is a security privileged process for the **OpenClaw** ecosystem. It sits between OpenClaw and upstream LLM API providers (OpenAI, Anthropic), performing virtual-to-real API key mapping and DLP (Data Loss Prevention) scanning on request and response bodies.

OpenClaw never holds real API keys — only virtual keys that ClawShell swaps for real ones before forwarding requests upstream. Real keys are stored in a privileged config directory (`/etc/clawshell`) protected by Unix file system permissions.

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

- **Transparent Proxy**: Deploys alongside OpenClaw without requiring code changes — configure OpenClaw to point at ClawShell's address and it forwards all requests upstream.
- **No External Dependencies**: Uses Unix file system permissions to protect secrets. No IdP, Vault, or external key management service required.
  

### 4. Ultra Lightweight and Scalable
- Runs in under 10MB of memory.
- Written in Rust with Tokio

## Architecture

```
                               ║ security boundary (Unix File System Permissions)
                               ║
                               ║  ┌─────────────────-─┐
                               ║  │  /etc/clawshell   │
                               ║  │  ┄ real API keys  │
                               ║  │  ┄ DLP patterns   │
                               ║  └────────┬─────────-┘
                               ║     reads │
                               ║  ┌────────┴─────────-┐
  ┌──────────────┐  REQUEST    ║  │                   │   REQUEST       ┌────────────┐
  │              ├──(virtual───╫─►│    ClawShell      ├──-(real key,───►│            │
  │   OpenClaw   │   key)      ║  │                   │   PII redacted) │   OpenAI   │
  │              │             ║  │  DLP scan         │                 │     or     │
  │ holds only   │  RESPONSE   ║  │  real-key mapping │   RESPONSE      │  Anthropic │
  │ virtual keys │◄─-----------║◄─┤                   │◄─-----------────┤            │
  │              │             ║  │                   │                 │            │
  └──────────────┘             ║  └──────────────────-┘                 └────────────┘
                               ║
```

OpenClaw only holds virtual keys and cannot access the
real API keys stored in the privileged config.
ClawShell swaps virtual keys for real ones and
scans for PII before forwarding requests upstream.

## Installation

### Cargo

```bash
cargo install clawshell --locked

# Require privilege to setup security boundary
sudo clawshell onboard
```

## NPM

```bash
npm install -g @clawshell/clawshell

# Require privilege to setup security boundary
sudo clawshell onboard
```

## Build from Source

```bash
cargo build --release
ls -al target/release/clawshell
```

### Cross-compile on Linux/arm64

```bash
wget https://musl.cc/x86_64-linux-musl-cross.tgz -O /tmp/musl-cross.tgz
tar -xzf /tmp/musl-cross.tgz -C /tmp
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="/tmp/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc" \
cargo build --release --target x86_64-unknown-linux-musl
```
