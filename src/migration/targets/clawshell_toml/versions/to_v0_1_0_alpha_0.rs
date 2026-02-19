use crate::migration::core::{AmbiguityResolver, AmbiguousChoice, MigrationIssue};
use crate::migration::target::TargetError;

use super::VersionStepOutput;

const STEP_ID: &str = "rename `upstream.base_url` to `upstream.openai_base_url`";
const BOTH_KEYS_REASON: &str =
    "both `upstream.base_url` and `upstream.openai_base_url` are present";

pub fn apply(
    target_name: &str,
    table: &mut toml::value::Table,
    resolver: &mut dyn AmbiguityResolver,
) -> Result<VersionStepOutput, TargetError> {
    let mut output = VersionStepOutput::default();

    let Some(upstream_value) = table.get_mut("upstream") else {
        return Ok(output);
    };

    let upstream = upstream_value
        .as_table_mut()
        .ok_or_else(|| TargetError::Validation {
            details: "[upstream] must be a table".to_string(),
        })?;

    let has_legacy = upstream.contains_key("base_url");
    let has_openai = upstream.contains_key("openai_base_url");

    if has_legacy && !has_openai {
        let legacy = upstream.remove("base_url").expect("checked contains_key");
        upstream.insert("openai_base_url".to_string(), legacy);
        output.applied_steps.push(STEP_ID.to_string());
    } else if has_legacy && has_openai {
        let issue = MigrationIssue {
            target: target_name.to_string(),
            step_id: STEP_ID.to_string(),
            message:
                "both upstream.base_url and upstream.openai_base_url are present; cannot choose automatically"
                    .to_string(),
            recommended: AmbiguousChoice::Abort,
        };

        match resolver.resolve(&issue)? {
            AmbiguousChoice::ApplyRecommended | AmbiguousChoice::Abort => {
                return Err(TargetError::MigrationAborted {
                    reason: BOTH_KEYS_REASON.to_string(),
                });
            }
            AmbiguousChoice::Skip => {
                upstream.remove("base_url");
                output
                    .applied_steps
                    .push("drop-legacy-upstream-base-url".to_string());
                output.warnings.push(
                    "Dropped legacy upstream.base_url and kept upstream.openai_base_url."
                        .to_string(),
                );
            }
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::core::{AmbiguityResolutionError, AmbiguousChoice, MigrationIssue};

    #[derive(Debug)]
    struct AbortResolver;

    impl AmbiguityResolver for AbortResolver {
        fn resolve(
            &mut self,
            _issue: &MigrationIssue,
        ) -> Result<AmbiguousChoice, AmbiguityResolutionError> {
            Ok(AmbiguousChoice::ApplyRecommended)
        }
    }

    #[derive(Debug)]
    struct SkipResolver;

    impl AmbiguityResolver for SkipResolver {
        fn resolve(
            &mut self,
            _issue: &MigrationIssue,
        ) -> Result<AmbiguousChoice, AmbiguityResolutionError> {
            Ok(AmbiguousChoice::Skip)
        }
    }

    #[test]
    fn test_apply_renames_legacy_key() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
[upstream]
base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let mut resolver = AbortResolver;
        let output = apply("clawshell", &mut table, &mut resolver).unwrap();

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
    fn test_apply_aborts_when_both_keys_exist_by_default() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
[upstream]
base_url = "https://legacy.openai.com"
openai_base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let mut resolver = AbortResolver;
        let err = apply("clawshell", &mut table, &mut resolver).expect_err("expected abort");
        assert!(
            err.to_string()
                .contains("both `upstream.base_url` and `upstream.openai_base_url` are present")
        );
    }

    #[test]
    fn test_apply_skip_drops_legacy_when_both_keys_exist() {
        let mut table: toml::value::Table = toml::from_str(
            r#"
[upstream]
base_url = "https://legacy.openai.com"
openai_base_url = "https://api.openai.com"
"#,
        )
        .unwrap();

        let mut resolver = SkipResolver;
        let output = apply("clawshell", &mut table, &mut resolver).unwrap();

        assert!(
            output
                .applied_steps
                .iter()
                .any(|s| s == "drop-legacy-upstream-base-url")
        );
        assert!(
            output
                .warnings
                .iter()
                .any(|w| w.contains("Dropped legacy upstream.base_url"))
        );

        let upstream = table.get("upstream").unwrap().as_table().unwrap();
        assert!(upstream.get("openai_base_url").is_some());
        assert!(upstream.get("base_url").is_none());
    }
}
