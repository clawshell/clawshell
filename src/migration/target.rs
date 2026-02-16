use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::migration::core::{
    AmbiguityResolutionError, AmbiguityResolver, ConfigVersion, ConfigVersionParseError,
};

#[derive(Debug, Clone, Default)]
pub struct TargetMigrationOutput {
    pub content: String,
    pub applied_steps: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum TargetError {
    #[error("failed to parse TOML: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("failed to serialize TOML: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("configuration must be a top-level TOML table")]
    TopLevelTableExpected,

    #[error("top-level 'version' must be a string, got {found}")]
    VersionMustBeString { found: String },

    #[error("invalid top-level 'version' value '{value}': {source}")]
    InvalidVersionValue {
        value: String,
        #[source]
        source: ConfigVersionParseError,
    },

    #[error("configuration migration required: missing top-level 'version' (expected {expected})")]
    MigrationRequiredMissingVersion {
        expected: ConfigVersion,
        path: PathBuf,
    },

    #[error(
        "configuration migration required: found version {found} but current version is {expected}"
    )]
    MigrationRequiredVersionMismatch {
        found: ConfigVersion,
        expected: ConfigVersion,
        path: PathBuf,
    },

    #[error("migration aborted: source version {from} is newer than target {to}")]
    DowngradeAborted {
        from: ConfigVersion,
        to: ConfigVersion,
    },

    #[error("migration aborted: {reason}")]
    MigrationAborted { reason: String },

    #[error("invalid configuration after migration: {details}")]
    Validation { details: String },

    #[error(transparent)]
    Ambiguity(#[from] AmbiguityResolutionError),
}

pub trait MigrationTarget: std::fmt::Debug {
    fn name(&self) -> &'static str;
    fn path(&self) -> &Path;

    fn detect_version(&self, content: &str) -> Result<Option<ConfigVersion>, TargetError>;

    fn migrate(
        &self,
        content: &str,
        from: &ConfigVersion,
        to: &ConfigVersion,
        resolver: &mut dyn AmbiguityResolver,
    ) -> Result<TargetMigrationOutput, TargetError>;

    fn validate(&self, content: &str) -> Result<(), TargetError>;
}
