use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::migration::core::{AmbiguityResolver, AmbiguousChoice, ConfigVersion, MigrationIssue};
use crate::migration::target::{MigrationTarget, TargetError, TargetMigrationOutput};

mod versions;

#[derive(Debug, Clone)]
pub struct ClawshellTomlTarget {
    path: PathBuf,
}

impl ClawshellTomlTarget {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionGateStatus {
    Current(ConfigVersion),
    Missing,
    Mismatch { found: ConfigVersion },
}

pub fn version_gate_status(path: &Path) -> Result<VersionGateStatus, TargetError> {
    let content = std::fs::read_to_string(path).map_err(|e| TargetError::Validation {
        details: format!("failed to read '{}': {}", path.display(), e),
    })?;
    version_gate_status_from_content(&content)
}

pub fn version_gate_status_from_content(content: &str) -> Result<VersionGateStatus, TargetError> {
    let expected = ConfigVersion::current();
    let found = detect_version_from_content(content)?;

    let status = match found {
        Some(version) if version == expected => VersionGateStatus::Current(version),
        Some(version) => VersionGateStatus::Mismatch { found: version },
        None => VersionGateStatus::Missing,
    };

    Ok(status)
}

pub fn ensure_current_version(path: &Path) -> Result<(), TargetError> {
    let expected = ConfigVersion::current();
    match version_gate_status(path)? {
        VersionGateStatus::Current(_) => Ok(()),
        VersionGateStatus::Missing => Err(TargetError::MigrationRequiredMissingVersion {
            expected,
            path: path.to_path_buf(),
        }),
        VersionGateStatus::Mismatch { found } => {
            Err(TargetError::MigrationRequiredVersionMismatch {
                found,
                expected,
                path: path.to_path_buf(),
            })
        }
    }
}

pub fn detect_version_from_content(content: &str) -> Result<Option<ConfigVersion>, TargetError> {
    let parsed: toml::Value = toml::from_str(content)?;
    let table = parsed
        .as_table()
        .ok_or(TargetError::TopLevelTableExpected)?;

    let Some(version_value) = table.get("version") else {
        return Ok(None);
    };

    let Some(version_str) = version_value.as_str() else {
        return Err(TargetError::VersionMustBeString {
            found: version_value.type_str().to_string(),
        });
    };

    let parsed =
        version_str
            .parse::<ConfigVersion>()
            .map_err(|e| TargetError::InvalidVersionValue {
                value: version_str.to_string(),
                source: e,
            })?;

    Ok(Some(parsed))
}

impl MigrationTarget for ClawshellTomlTarget {
    fn name(&self) -> &'static str {
        "clawshell"
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn detect_version(&self, content: &str) -> Result<Option<ConfigVersion>, TargetError> {
        detect_version_from_content(content)
    }

    fn migrate(
        &self,
        content: &str,
        from: &ConfigVersion,
        to: &ConfigVersion,
        resolver: &mut dyn AmbiguityResolver,
    ) -> Result<TargetMigrationOutput, TargetError> {
        if from > to {
            let issue = MigrationIssue {
                target: self.name().to_string(),
                step_id: "future-version".to_string(),
                message: format!(
                    "source version {} is newer than target {} (downgrade is unsupported)",
                    from, to
                ),
                recommended: AmbiguousChoice::Abort,
            };
            return match resolver.resolve(&issue)? {
                AmbiguousChoice::ApplyRecommended | AmbiguousChoice::Abort => {
                    Err(TargetError::DowngradeAborted {
                        from: from.clone(),
                        to: to.clone(),
                    })
                }
                AmbiguousChoice::Skip => Ok(TargetMigrationOutput {
                    content: content.to_string(),
                    applied_steps: vec!["skip-future-version".to_string()],
                    warnings: vec![format!(
                        "skipped migration because source version {} is newer than target {}",
                        from, to
                    )],
                }),
            };
        }

        let detected = detect_version_from_content(content)?;
        let mut value: toml::Value = toml::from_str(content)?;
        let table = value
            .as_table_mut()
            .ok_or(TargetError::TopLevelTableExpected)?;
        let version_step_output =
            versions::apply_versioned_steps(self.name(), table, from, to, resolver)?;
        let mut applied_steps = version_step_output.applied_steps;
        let warnings = version_step_output.warnings;

        if detected.as_ref() != Some(to) {
            table.insert("version".to_string(), toml::Value::String(to.to_string()));
            let step = match detected {
                Some(found) => format!("set-version:{}->{}", found, to),
                None => format!("set-version:missing->{}", to),
            };
            applied_steps.push(step);
        }

        let migrated = toml::to_string_pretty(&value)?;

        Ok(TargetMigrationOutput {
            content: migrated,
            applied_steps,
            warnings,
        })
    }

    fn validate(&self, content: &str) -> Result<(), TargetError> {
        Config::from_str_with_validation(content).map_err(|e| TargetError::Validation {
            details: e.to_string(),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_version_from_content_missing() {
        let content = r#"
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://api.openai.com"
"#;

        let version = detect_version_from_content(content).unwrap();
        assert!(version.is_none());
    }

    #[test]
    fn test_detect_version_from_content_present() {
        let content = r#"
version = "0.0.1"
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://api.openai.com"
"#;

        let version = detect_version_from_content(content).unwrap().unwrap();
        assert_eq!(version.to_string(), "0.0.1");
    }

    #[test]
    fn test_version_gate_status_missing() {
        let content = r#"
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://api.openai.com"
"#;

        let status = version_gate_status_from_content(content).unwrap();
        assert_eq!(status, VersionGateStatus::Missing);
    }

    #[test]
    fn test_migrate_stamps_version_when_missing() {
        let target = ClawshellTomlTarget::new(PathBuf::from("/tmp/clawshell.toml"));
        let content = r#"
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://api.openai.com"
"#;

        #[derive(Debug)]
        struct NoopResolver;

        impl AmbiguityResolver for NoopResolver {
            fn resolve(
                &mut self,
                _issue: &crate::migration::core::MigrationIssue,
            ) -> Result<
                crate::migration::core::AmbiguousChoice,
                crate::migration::core::AmbiguityResolutionError,
            > {
                Ok(crate::migration::core::AmbiguousChoice::ApplyRecommended)
            }
        }

        let mut resolver = NoopResolver;
        let from = ConfigVersion::legacy_baseline();
        let to = ConfigVersion::current();
        let result = target.migrate(content, &from, &to, &mut resolver).unwrap();
        assert!(result.content.contains("version = \""));
        assert!(
            result
                .content
                .contains("openai_base_url = \"https://api.openai.com\"")
        );
        assert!(!result.content.contains("\nbase_url = "));
        assert!(
            result
                .applied_steps
                .iter()
                .any(|s| s == "rename `upstream.base_url` to `upstream.openai_base_url`")
        );
    }

    #[test]
    fn test_migrate_with_both_upstream_keys_aborts_by_default() {
        let target = ClawshellTomlTarget::new(PathBuf::from("/tmp/clawshell.toml"));
        let content = r#"
version = "0.0.1"
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://legacy.openai.com"
openai_base_url = "https://api.openai.com"
"#;

        #[derive(Debug)]
        struct AbortResolver;

        impl AmbiguityResolver for AbortResolver {
            fn resolve(
                &mut self,
                _issue: &crate::migration::core::MigrationIssue,
            ) -> Result<
                crate::migration::core::AmbiguousChoice,
                crate::migration::core::AmbiguityResolutionError,
            > {
                Ok(crate::migration::core::AmbiguousChoice::ApplyRecommended)
            }
        }

        let mut resolver = AbortResolver;
        let from: ConfigVersion = "0.0.1".parse().unwrap();
        let to = ConfigVersion::current();
        let err = target
            .migrate(content, &from, &to, &mut resolver)
            .expect_err("expected ambiguity to abort migration");
        assert!(
            err.to_string()
                .contains("both `upstream.base_url` and `upstream.openai_base_url` are present")
        );
    }

    #[test]
    fn test_migrate_with_both_upstream_keys_skip_drops_legacy() {
        let target = ClawshellTomlTarget::new(PathBuf::from("/tmp/clawshell.toml"));
        let content = r#"
version = "0.0.1"
log_level = "info"

[server]
host = "127.0.0.1"
port = 18790

[upstream]
base_url = "https://legacy.openai.com"
openai_base_url = "https://api.openai.com"
"#;

        #[derive(Debug)]
        struct SkipResolver;

        impl AmbiguityResolver for SkipResolver {
            fn resolve(
                &mut self,
                _issue: &crate::migration::core::MigrationIssue,
            ) -> Result<
                crate::migration::core::AmbiguousChoice,
                crate::migration::core::AmbiguityResolutionError,
            > {
                Ok(crate::migration::core::AmbiguousChoice::Skip)
            }
        }

        let mut resolver = SkipResolver;
        let from: ConfigVersion = "0.0.1".parse().unwrap();
        let to = ConfigVersion::current();
        let result = target.migrate(content, &from, &to, &mut resolver).unwrap();
        assert!(
            result
                .content
                .contains("openai_base_url = \"https://api.openai.com\"")
        );
        assert!(!result.content.contains("\nbase_url = "));
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("Dropped legacy upstream.base_url"))
        );
    }
}
