# Running ClawShell in Docker

## Build

Create a `.env` file in the project root with your credentials (required for
Antigravity/Google OAuth):

```
GOOGLE_OAUTH_CLIENT_ID=your-client-id.apps.googleusercontent.com
GOOGLE_OAUTH_CLIENT_SECRET=your-client-secret
```

Then build the release binary and Docker image:

```bash
cargo build --release
docker build -t clawshell .
```

The `.env` file is baked into the image at `/etc/clawshell/.env` and loaded
automatically at runtime — no need to pass `--env-file` or `-e` flags.

> **Security note:** The `.env` file is embedded in the image. Do not push
> the image to a public registry if it contains sensitive credentials.

## Onboarding

Run the interactive onboard wizard to generate configuration:

```bash
docker run --rm -it clawshell onboard
```

If you're onboarding with Antigravity/Google OAuth, publish the callback port:

```bash
docker run --rm -it \
  -p 51121:51121 \
  clawshell onboard
```

This creates the configuration files inside the container. To persist them,
mount a volume for `/etc/clawshell`:

```bash
docker run --rm -it -v clawshell-config:/etc/clawshell clawshell onboard
```

The wizard will prompt you to select a provider (OpenAI, OpenRouter, Anthropic,
Codex/ChatGPT OAuth, or Antigravity/Google OAuth), a model, and an API key or
OAuth login.

## Running the proxy

Start ClawShell in the foreground with the persisted configuration:

```bash
docker run -d \
  --name clawshell \
  -p 18790:18790 \
  -v clawshell-config:/etc/clawshell \
  clawshell start --foreground
```

The proxy listens on port `18790` by default. The `--foreground` flag is
required in Docker (no daemonization).

### Binding to all interfaces

By default ClawShell listens on `127.0.0.1`, which is unreachable from outside
the container. Set the host to `0.0.0.0` in your `clawshell.toml`:

```toml
[server]
host = "0.0.0.0"
port = 18790
```

Or pass it during onboard when prompted for the server host.

You can also override server bind host/port at runtime:

```bash
docker run -d \
  --name clawshell \
  -p 17890:17890 \
  -e CLAWSHELL_SERVER_HOST=0.0.0.0 \
  -e CLAWSHELL_SERVER_PORT=17890 \
  -v clawshell-config:/etc/clawshell \
  clawshell start --foreground
```

## Configuration volume

All ClawShell state lives under `/etc/clawshell`:

| Path                            | Purpose                              |
|---------------------------------|--------------------------------------|
| `/etc/clawshell/clawshell.toml` | Main configuration file              |
| `/etc/clawshell/config.json`    | Onboard metadata                     |
| `/etc/clawshell/oauth/`         | OAuth token files (0600 perms)       |
| `/etc/clawshell/.env`           | Google OAuth credentials (from build)|

Use a named volume (`clawshell-config`) or a bind mount to persist these across
container restarts.

## Environment variables

The `.env` file is copied into the image at build time and loaded automatically.
You can also override values at runtime if needed:

```bash
docker run --rm -it \
  -e GOOGLE_OAUTH_CLIENT_ID=different-id.apps.googleusercontent.com \
  clawshell onboard
```

Runtime `-e` flags take precedence over the baked-in `.env` file.

### Runtime server bind overrides

| Variable                 | Description                               |
|--------------------------|-------------------------------------------|
| `CLAWSHELL_SERVER_HOST`  | Overrides `[server].host` (e.g. `0.0.0.0`) |
| `CLAWSHELL_SERVER_PORT`  | Overrides `[server].port` (e.g. `17890`)   |

### Required variables for Antigravity / Google OAuth

| Variable                      | Description                |
|-------------------------------|----------------------------|
| `GOOGLE_OAUTH_CLIENT_ID`     | Google OAuth client ID     |
| `GOOGLE_OAUTH_CLIENT_SECRET` | Google OAuth client secret |

These are not needed for other providers (OpenAI, OpenRouter, Anthropic, Codex).

## OAuth providers

### Codex / ChatGPT (OAuth)

Uses device code flow — no browser required inside the container. The wizard
prints a URL and a one-time code. Open the URL on any device, enter the code,
and the container receives the tokens automatically.

No extra environment variables are needed for Codex.

### Antigravity / Google (OAuth)

Requires `GOOGLE_OAUTH_CLIENT_ID` and `GOOGLE_OAUTH_CLIENT_SECRET` (provided
via the `.env` file baked into the image at build time).

Uses a localhost callback flow on port `51121`. During onboarding:

1. Run the container with `-p 51121:51121`.
2. Open the printed Google authorization URL.
3. Complete consent; Google redirects to `http://localhost:51121/oauth-callback...`.

The `Dockerfile` sets `CLAWSHELL_OAUTH_CALLBACK_HOST=0.0.0.0` so the callback
listener inside the container accepts the published port.

## Stopping

```bash
docker stop clawshell
```

## Example: full setup

```bash
# 1. Create .env with Google OAuth credentials (skip if not using Antigravity)
cat > .env << 'EOF'
GOOGLE_OAUTH_CLIENT_ID=your-client-id.apps.googleusercontent.com
GOOGLE_OAUTH_CLIENT_SECRET=your-client-secret
EOF

# 2. Build
cargo build --release
docker build -t clawshell .

# 3. Onboard (interactive — creates config in the volume)
#    Add -p 51121:51121 if using Antigravity/Google OAuth.
docker run --rm -it -v clawshell-config:/etc/clawshell -p 51121:51121 clawshell onboard

# 4. Run
docker run -d \
  --name clawshell \
  --restart unless-stopped \
  -p 18790:18790 \
  -v clawshell-config:/etc/clawshell \
  clawshell start --foreground

# 5. Verify
curl http://localhost:18790/health
```
