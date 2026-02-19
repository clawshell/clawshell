use std::fmt;
use std::str::FromStr;

use semver::Version;
use thiserror::Error;

pub const LEGACY_BASELINE_VERSION: &str = "0.0.1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfigVersion(Version);

impl ConfigVersion {
    pub fn current() -> Self {
        // Safe: package version is required by Cargo and validated at compile time.
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("CARGO_PKG_VERSION must be a semantic version")
    }

    pub fn legacy_baseline() -> Self {
        LEGACY_BASELINE_VERSION
            .parse()
            .expect("legacy baseline version must be valid")
    }
}

impl fmt::Display for ConfigVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "invalid semantic version '{raw}' (expected SemVer: MAJOR.MINOR.PATCH with optional -PRERELEASE)"
)]
pub struct ConfigVersionParseError {
    raw: String,
}

impl FromStr for ConfigVersion {
    type Err = ConfigVersionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let raw = value.trim();
        Version::parse(raw)
            .map(Self)
            .map_err(|_| ConfigVersionParseError {
                raw: raw.to_string(),
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbiguousChoice {
    ApplyRecommended,
    Skip,
    Abort,
}

impl fmt::Display for AmbiguousChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AmbiguousChoice::ApplyRecommended => write!(f, "apply recommended"),
            AmbiguousChoice::Skip => write!(f, "skip"),
            AmbiguousChoice::Abort => write!(f, "abort"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MigrationIssue {
    pub target: String,
    pub step_id: String,
    pub message: String,
    pub recommended: AmbiguousChoice,
}

#[derive(Debug, Clone, Error)]
pub enum AmbiguityResolutionError {
    #[error("{0}")]
    Message(String),
}

pub trait AmbiguityResolver: std::fmt::Debug {
    fn resolve(
        &mut self,
        issue: &MigrationIssue,
    ) -> Result<AmbiguousChoice, AmbiguityResolutionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config_version_valid() {
        let version: ConfigVersion = "1.2.3".parse().unwrap();
        assert_eq!(version.to_string(), "1.2.3");
    }

    #[test]
    fn test_parse_config_version_valid_prerelease() {
        let version: ConfigVersion = "0.1.0-alpha.0".parse().unwrap();
        assert_eq!(version.to_string(), "0.1.0-alpha.0");
    }

    #[test]
    fn test_parse_config_version_invalid() {
        assert!("1.2".parse::<ConfigVersion>().is_err());
        assert!("1.2.3.4".parse::<ConfigVersion>().is_err());
        assert!("1.2.alpha".parse::<ConfigVersion>().is_err());
        assert!("alpha".parse::<ConfigVersion>().is_err());
        assert!("0.1.0.alpha.0".parse::<ConfigVersion>().is_err());
    }

    #[test]
    fn test_version_ordering() {
        let v1: ConfigVersion = "0.0.1".parse().unwrap();
        let v2: ConfigVersion = "0.1.0-alpha.0".parse().unwrap();
        let v3: ConfigVersion = "0.1.0-alpha.1".parse().unwrap();
        let v4: ConfigVersion = "0.1.0".parse().unwrap();
        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v3 < v4);
    }
}
