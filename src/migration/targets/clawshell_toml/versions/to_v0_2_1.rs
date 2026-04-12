use crate::migration::core::AmbiguityResolver;
use crate::migration::target::TargetError;

use super::VersionStepOutput;

const STEP_ID: &str = "add `[stats]` section with default persist_path";

pub fn apply(
    _target_name: &str,
    table: &mut toml::value::Table,
    _resolver: &mut dyn AmbiguityResolver,
) -> Result<VersionStepOutput, TargetError> {
    let mut output = VersionStepOutput::default();

    if !table.contains_key("stats") {
        let mut stats = toml::value::Table::new();
        stats.insert(
            "persist_path".to_string(),
            toml::Value::String("/etc/clawshell/stats.json".to_string()),
        );
        table.insert("stats".to_string(), toml::Value::Table(stats));
        output.applied_steps.push(STEP_ID.to_string());
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::core::{AmbiguityResolutionError, AmbiguousChoice, MigrationIssue};

    #[derive(Debug)]
    struct NoopResolver;

    impl AmbiguityResolver for NoopResolver {
        fn resolve(
            &mut self,
            _issue: &MigrationIssue,
        ) -> Result<AmbiguousChoice, AmbiguityResolutionError> {
            Ok(AmbiguousChoice::ApplyRecommended)
        }
    }

    #[test]
    fn test_injects_stats_when_missing() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
[upstream]
openai_base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let mut resolver = NoopResolver;
        let output = apply("clawshell", &mut table, &mut resolver).unwrap();
        assert_eq!(output.applied_steps.len(), 1);
        assert!(output.applied_steps[0].contains("[stats]"));

        let stats = table.get("stats").unwrap().as_table().unwrap();
        assert_eq!(
            stats.get("persist_path").unwrap().as_str().unwrap(),
            "/etc/clawshell/stats.json"
        );
    }

    #[test]
    fn test_skips_when_stats_already_present() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
[upstream]
openai_base_url = "https://api.openai.com"
[stats]
persist_path = "/custom/path/stats.json"
"#,
        )
        .unwrap();

        let mut resolver = NoopResolver;
        let output = apply("clawshell", &mut table, &mut resolver).unwrap();
        assert!(output.applied_steps.is_empty());

        let stats = table.get("stats").unwrap().as_table().unwrap();
        assert_eq!(
            stats.get("persist_path").unwrap().as_str().unwrap(),
            "/custom/path/stats.json"
        );
    }
}
