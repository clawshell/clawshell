mod to_v0_1_0_alpha_0;
mod to_v0_2_1;

use crate::migration::core::{AmbiguityResolver, ConfigVersion};
use crate::migration::target::TargetError;

#[derive(Debug, Default)]
pub struct VersionStepOutput {
    pub applied_steps: Vec<String>,
    pub warnings: Vec<String>,
}

impl VersionStepOutput {
    fn merge(&mut self, mut other: VersionStepOutput) {
        self.applied_steps.append(&mut other.applied_steps);
        self.warnings.append(&mut other.warnings);
    }
}

pub fn apply_versioned_steps(
    target_name: &str,
    table: &mut toml::value::Table,
    from: &ConfigVersion,
    to: &ConfigVersion,
    resolver: &mut dyn AmbiguityResolver,
) -> Result<VersionStepOutput, TargetError> {
    let mut output = VersionStepOutput::default();
    let v0_1_0_alpha_0: ConfigVersion = "0.1.0-alpha.0"
        .parse()
        .expect("literal config version must parse");

    if from < &v0_1_0_alpha_0 && to >= &v0_1_0_alpha_0 {
        let step_output = to_v0_1_0_alpha_0::apply(target_name, table, resolver)?;
        output.merge(step_output);
    }

    let v0_2_1: ConfigVersion = "0.2.1".parse().expect("literal config version must parse");

    if from < &v0_2_1 && to >= &v0_2_1 {
        let step_output = to_v0_2_1::apply(target_name, table, resolver)?;
        output.merge(step_output);
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
    fn test_apply_versioned_steps_applies_on_crossing_to_0_1_0_alpha_0() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
version = "0.0.1"

[upstream]
base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let from: ConfigVersion = "0.0.1".parse().unwrap();
        let to: ConfigVersion = "0.1.0-alpha.0".parse().unwrap();

        let mut resolver = NoopResolver;
        let output = apply_versioned_steps("clawshell", &mut table, &from, &to, &mut resolver)
            .expect("migration should succeed");

        assert!(
            output
                .applied_steps
                .iter()
                .any(|s| s == "rename `upstream.base_url` to `upstream.openai_base_url`")
        );
        let upstream = table.get("upstream").unwrap().as_table().unwrap();
        assert!(upstream.get("openai_base_url").is_some());
        assert!(upstream.get("base_url").is_none());
    }

    #[test]
    fn test_apply_versioned_steps_noop_when_already_current() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
version = "0.1.0-alpha.0"

[upstream]
openai_base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let from: ConfigVersion = "0.1.0-alpha.0".parse().unwrap();
        let to: ConfigVersion = "0.1.0-alpha.0".parse().unwrap();

        let mut resolver = NoopResolver;
        let output = apply_versioned_steps("clawshell", &mut table, &from, &to, &mut resolver)
            .expect("migration should succeed");

        assert!(output.applied_steps.is_empty());
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn test_apply_versioned_steps_adds_stats_from_0_1_1() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
version = "0.1.1"

[server]
host = "127.0.0.1"
[upstream]
openai_base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let from: ConfigVersion = "0.1.1".parse().unwrap();
        let to: ConfigVersion = "0.2.1".parse().unwrap();

        let mut resolver = NoopResolver;
        let output = apply_versioned_steps("clawshell", &mut table, &from, &to, &mut resolver)
            .expect("migration should succeed");

        assert!(output.applied_steps.iter().any(|s| s.contains("[stats]")));
        let stats = table.get("stats").unwrap().as_table().unwrap();
        assert_eq!(
            stats.get("persist_path").unwrap().as_str().unwrap(),
            "/etc/clawshell/stats.json"
        );
    }

    #[test]
    fn test_apply_versioned_steps_adds_stats_from_0_2_0() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
version = "0.2.0"

[server]
host = "127.0.0.1"
[upstream]
openai_base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let from: ConfigVersion = "0.2.0".parse().unwrap();
        let to: ConfigVersion = "0.2.1".parse().unwrap();

        let mut resolver = NoopResolver;
        let output = apply_versioned_steps("clawshell", &mut table, &from, &to, &mut resolver)
            .expect("migration should succeed");

        assert!(output.applied_steps.iter().any(|s| s.contains("[stats]")));
        assert!(table.contains_key("stats"));
    }
}
