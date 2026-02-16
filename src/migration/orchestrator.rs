use std::fs;
use std::path::{Path, PathBuf};

use crate::migration::core::{AmbiguityResolver, ConfigVersion};
use crate::migration::target::{MigrationTarget, TargetError, TargetMigrationOutput};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct TargetMigrationReport {
    pub target_name: String,
    pub path: PathBuf,
    pub from_version: ConfigVersion,
    pub to_version: ConfigVersion,
    pub changed: bool,
    pub backup_path: Option<PathBuf>,
    pub applied_steps: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MigrationReport {
    pub to_version: ConfigVersion,
    pub targets: Vec<TargetMigrationReport>,
}

#[derive(Debug)]
struct PendingMigration {
    target_name: String,
    path: PathBuf,
    from_version: ConfigVersion,
    to_version: ConfigVersion,
    output: TargetMigrationOutput,
    changed: bool,
    backup_path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum MigrationOrchestratorError {
    #[error("failed to read '{path}': {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("target '{target}' failed version detection: {source}")]
    DetectVersion {
        target: String,
        #[source]
        source: TargetError,
    },

    #[error("target '{target}' migration failed: {source}")]
    MigrateTarget {
        target: String,
        #[source]
        source: TargetError,
    },

    #[error("target '{target}' validation failed: {source}")]
    ValidateTarget {
        target: String,
        #[source]
        source: TargetError,
    },

    #[error("failed to create backup '{backup}' for '{path}': {source}")]
    BackupFile {
        path: PathBuf,
        backup: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write '{path}': {source}{rollback_suffix}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
        rollback_suffix: String,
    },
}

pub fn migrate_targets(
    targets: &[Box<dyn MigrationTarget>],
    resolver: &mut dyn AmbiguityResolver,
) -> Result<MigrationReport, MigrationOrchestratorError> {
    let to_version = ConfigVersion::current();
    let mut pending = Vec::new();

    for target in targets {
        let path = target.path().to_path_buf();
        let target_name = target.name().to_string();
        let original =
            fs::read_to_string(&path).map_err(|source| MigrationOrchestratorError::ReadFile {
                path: path.clone(),
                source,
            })?;
        let detected = target.detect_version(&original).map_err(|source| {
            MigrationOrchestratorError::DetectVersion {
                target: target_name.clone(),
                source,
            }
        })?;
        let from_version = detected.unwrap_or_else(ConfigVersion::legacy_baseline);
        let output = target
            .migrate(&original, &from_version, &to_version, resolver)
            .map_err(|source| MigrationOrchestratorError::MigrateTarget {
                target: target_name.clone(),
                source,
            })?;

        target.validate(&output.content).map_err(|source| {
            MigrationOrchestratorError::ValidateTarget {
                target: target_name.clone(),
                source,
            }
        })?;

        pending.push(PendingMigration {
            target_name,
            path,
            from_version,
            to_version: to_version.clone(),
            changed: output.content != original,
            output,
            backup_path: None,
        });
    }

    let mut written = Vec::<(PathBuf, PathBuf)>::new();

    for entry in &mut pending {
        if !entry.changed {
            continue;
        }

        let backup_path = next_backup_path(&entry.path);
        fs::copy(&entry.path, &backup_path).map_err(|source| {
            MigrationOrchestratorError::BackupFile {
                path: entry.path.clone(),
                backup: backup_path.clone(),
                source,
            }
        })?;

        if let Err(source) = fs::write(&entry.path, &entry.output.content) {
            let rollback_errors = rollback_written(&written);
            let rollback_suffix = if rollback_errors.is_empty() {
                String::new()
            } else {
                format!("; rollback errors: {}", rollback_errors.join(" | "))
            };
            return Err(MigrationOrchestratorError::WriteFile {
                path: entry.path.clone(),
                source,
                rollback_suffix,
            });
        }

        written.push((entry.path.clone(), backup_path.clone()));
        entry.backup_path = Some(backup_path);
    }

    let targets = pending
        .into_iter()
        .map(|entry| TargetMigrationReport {
            target_name: entry.target_name,
            path: entry.path,
            from_version: entry.from_version,
            to_version: entry.to_version,
            changed: entry.changed,
            backup_path: entry.backup_path,
            applied_steps: entry.output.applied_steps,
            warnings: entry.output.warnings,
        })
        .collect();

    Ok(MigrationReport {
        to_version,
        targets,
    })
}

fn rollback_written(written: &[(PathBuf, PathBuf)]) -> Vec<String> {
    let mut errors = Vec::new();

    for (path, backup) in written.iter().rev() {
        if let Err(e) = fs::copy(backup, path) {
            errors.push(format!(
                "failed to restore '{}' from '{}': {}",
                path.display(),
                backup.display(),
                e
            ));
        }
    }

    errors
}

pub(crate) fn next_backup_path(path: &Path) -> PathBuf {
    let first = PathBuf::from(format!("{}.bak", path.display()));
    if !first.exists() {
        return first;
    }

    let mut idx = 1;
    loop {
        let candidate = PathBuf::from(format!("{}.bak.{}", path.display(), idx));
        if !candidate.exists() {
            return candidate;
        }
        idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_backup_path() {
        let unique = format!(
            "clawshell-migration-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();

        let file = root.join("config.toml");
        std::fs::write(&file, "version = \"0.0.1\"\n").unwrap();

        let bak0 = next_backup_path(&file);
        assert_eq!(bak0, PathBuf::from(format!("{}.bak", file.display())));

        std::fs::write(&bak0, "backup 0").unwrap();
        let bak1 = next_backup_path(&file);
        assert_eq!(bak1, PathBuf::from(format!("{}.bak.1", file.display())));

        std::fs::remove_dir_all(root).unwrap();
    }
}
