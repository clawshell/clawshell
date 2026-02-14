# Contributing to ClawShell

## Prerequisites

- Rust (stable toolchain)
- `cargo-insta` for snapshot review: `cargo install cargo-insta`

## Running Tests

Run the full test suite:

```sh
cargo test
```

This executes three test binaries:

| Binary | Source | What it covers |
|---|---|---|
| `integration` | `tests/integration.rs` | End-to-end proxy, DLP scanning, key mapping, AppState |
| `config_fixtures` | `tests/config_fixtures.rs` | Config parsing via fixture files + insta snapshots |
| `cli_tests` | `tests/cli_tests.rs` | CLI argument handling |

### Config Fixture Tests

Config parsing is tested with a data-driven approach using [`datatest-stable`](https://crates.io/crates/datatest-stable) and [`insta`](https://crates.io/crates/insta) snapshots.

**Structure:**

```
tests/
  config_fixtures.rs            # test harness (no need to edit for new cases)
  fixtures/config/
    valid/                      # configs that must parse successfully
      minimal.toml
      all_fields.toml
      ...
    invalid/                    # configs that must fail to parse
      missing_server.toml
      port_string.toml
      ...
  snapshots/                    # insta snapshots (auto-generated)
    config_fixtures__*.snap
```

- **Valid fixtures** are snapshot-tested against their full parsed output, including derived values (`listen_addr`, `upstream_url`, `key_map`).
- **Invalid fixtures** are snapshot-tested against their error messages.

**Adding a new config test case:**

1. Create a `.toml` file in `tests/fixtures/config/valid/` or `tests/fixtures/config/invalid/`.
2. Run the tests — new cases will fail because no snapshot exists yet:
   ```sh
   cargo test --test config_fixtures
   ```
3. Review and accept the new snapshots:
   ```sh
   cargo insta review
   ```
4. Commit the `.toml` fixture and the `.snap` snapshot file together.

**Updating snapshots after a config struct change:**

If you modify config structs or default values, existing snapshots will fail. Update them:

```sh
cargo insta test --review
```

This runs all tests, then opens an interactive review for any changed snapshots.

### Integration Tests

Integration tests in `tests/integration.rs` use [`wiremock`](https://crates.io/crates/wiremock) to mock upstream API servers. They cover:

- Proxy request forwarding and header injection
- Virtual-to-real key resolution
- DLP blocking and redaction (request and response)
- Streaming response passthrough
- Error handling (unknown keys, unsupported methods)

Run only integration tests:

```sh
cargo test --test integration
```

## Code Style

- Run `cargo fmt` before committing.
- Run `cargo clippy` and address warnings.
