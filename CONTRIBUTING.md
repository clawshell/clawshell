# Contributing to ClawShell

## Prerequisites

- Rust (stable toolchain)
- `cargo-insta` for snapshot review: `cargo install cargo-insta`

## Running Tests

Run the full test suite:

```sh
cargo test
```

This executes unit tests from `src/*` and integration tests from `tests/*`.

| Binary | Source | What it covers |
|---|---|---|
| `clawshell` | `src/*.rs` | Core behavior (config parsing/fixtures, migration helpers, proxy, app, onboarding) |
| `cli_tests` | `tests/cli_tests.rs` | CLI argument handling |

### Config Fixture Tests

Config parsing is tested with a data-driven approach using [`datatest-stable`](https://crates.io/crates/datatest-stable) and [`insta`](https://crates.io/crates/insta) snapshots.

**Structure:**

```
tests/
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
   cargo test config::tests::test_valid_config_fixtures
   cargo test config::tests::test_invalid_config_fixtures
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

## Code Style

- Run `cargo fmt` before committing.
- Run `cargo clippy` and address warnings.
