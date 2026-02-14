use clawshell::config::{Config, Provider};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Serialize)]
struct ConfigSnapshot {
    #[serde(flatten)]
    config: Config,
    derived: DerivedValues,
}

#[derive(Serialize)]
struct DerivedValues {
    listen_addr: String,
    openai_upstream_url: String,
    anthropic_upstream_url: String,
    key_map: BTreeMap<String, (String, Provider)>,
}

fn valid_config(path: &Path) -> datatest_stable::Result<()> {
    let config = Config::from_file(path)
        .map_err(|e| format!("expected valid config {}: {e}", path.display()))?;
    let snapshot = ConfigSnapshot {
        derived: DerivedValues {
            listen_addr: config.listen_addr(),
            openai_upstream_url: config.upstream_url(Provider::Openai),
            anthropic_upstream_url: config.upstream_url(Provider::Anthropic),
            key_map: config.key_map(),
        },
        config,
    };
    let name = path.file_stem().unwrap().to_str().unwrap();
    insta::assert_yaml_snapshot!(name, snapshot);
    Ok(())
}

fn invalid_config(path: &Path) -> datatest_stable::Result<()> {
    let err = Config::from_file(path).expect_err(&format!(
        "expected invalid config to fail: {}",
        path.display()
    ));
    let name = path.file_stem().unwrap().to_str().unwrap();
    insta::assert_snapshot!(name, err.to_string());
    Ok(())
}

datatest_stable::harness! {
    { test = valid_config, root = "tests/fixtures/config/valid", pattern = r"\.toml$" },
    { test = invalid_config, root = "tests/fixtures/config/invalid", pattern = r"\.toml$" },
}
