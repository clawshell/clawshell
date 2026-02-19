use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const MANAGED_BY_CLAWSHELL: &str = "clawshell";
const MANAGED_SKILL_SCHEMA_VERSION: u32 = 1;
const MANIFEST_CONFIG_KEY: &str = "managed_openclaw_skills";
pub const MANAGED_SKILL_METADATA_FILE_NAME: &str = ".clawshell-skill.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedSkillMetadata {
    pub managed_by: String,
    pub schema_version: u32,
    pub skill_name: String,
    pub install_id: String,
    pub created_at_unix_seconds: u64,
    pub files: Vec<String>,
    pub hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedSkillManifestEntry {
    pub schema_version: u32,
    pub skill_name: String,
    pub skill_dir: String,
    pub install_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedSkillUninstallState {
    Missing,
    ManagedUnchanged,
    ManagedModified,
    Unmanaged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSkillInspection {
    pub state: ManagedSkillUninstallState,
    pub detail: String,
}

impl ManagedSkillInspection {
    pub fn missing() -> Self {
        Self {
            state: ManagedSkillUninstallState::Missing,
            detail: "skill directory missing".to_string(),
        }
    }
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn hash_content(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    format!("{digest:x}")
}

fn as_unmanaged(detail: impl Into<String>) -> ManagedSkillInspection {
    ManagedSkillInspection {
        state: ManagedSkillUninstallState::Unmanaged,
        detail: detail.into(),
    }
}

fn as_managed_modified(detail: impl Into<String>) -> ManagedSkillInspection {
    ManagedSkillInspection {
        state: ManagedSkillUninstallState::ManagedModified,
        detail: detail.into(),
    }
}

fn canonicalize_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn build_managed_skill_metadata(
    skill_name: &str,
    managed_files: &[(String, String)],
) -> ManagedSkillMetadata {
    let mut files = Vec::with_capacity(managed_files.len());
    let mut hashes = BTreeMap::new();
    for (relative_path, content) in managed_files {
        files.push(relative_path.clone());
        hashes.insert(relative_path.clone(), hash_content(content.as_bytes()));
    }
    files.sort();
    files.dedup();

    ManagedSkillMetadata {
        managed_by: MANAGED_BY_CLAWSHELL.to_string(),
        schema_version: MANAGED_SKILL_SCHEMA_VERSION,
        skill_name: skill_name.to_string(),
        install_id: Uuid::new_v4().to_string(),
        created_at_unix_seconds: now_unix_seconds(),
        files,
        hashes,
    }
}

pub fn write_managed_skill_metadata(
    skill_dir: &Path,
    metadata: &ManagedSkillMetadata,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let metadata_path = skill_dir.join(MANAGED_SKILL_METADATA_FILE_NAME);
    let payload = serde_json::to_string_pretty(metadata)?;
    std::fs::write(&metadata_path, payload)?;
    Ok(metadata_path)
}

pub fn build_managed_skill_manifest_entry(
    skill_dir: &Path,
    metadata: &ManagedSkillMetadata,
) -> ManagedSkillManifestEntry {
    ManagedSkillManifestEntry {
        schema_version: metadata.schema_version,
        skill_name: metadata.skill_name.clone(),
        skill_dir: skill_dir.to_string_lossy().to_string(),
        install_id: metadata.install_id.clone(),
    }
}

pub fn upsert_managed_skill_manifest_entry(
    clawshell_config_path: &Path,
    entry: &ManagedSkillManifestEntry,
) -> Result<(), Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(clawshell_config_path)?;
    let mut json = serde_json::from_str::<serde_json::Value>(&content)?;
    let root = json.as_object_mut().ok_or_else(|| {
        format!(
            "Invalid ClawShell config JSON at {}: expected an object",
            clawshell_config_path.display()
        )
    })?;

    let manifest_node = root
        .entry(MANIFEST_CONFIG_KEY)
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !manifest_node.is_array() {
        *manifest_node = serde_json::Value::Array(Vec::new());
    }

    let manifest_items = manifest_node
        .as_array_mut()
        .expect("manifest_node must be array");
    manifest_items.retain(|item| {
        item.get("skill_name")
            .and_then(|value| value.as_str())
            .is_none_or(|skill_name| skill_name != entry.skill_name)
    });
    manifest_items.push(serde_json::to_value(entry)?);

    std::fs::write(clawshell_config_path, serde_json::to_string_pretty(&json)?)?;
    Ok(())
}

pub fn read_managed_skill_manifest_entry(
    clawshell_config_path: &Path,
    skill_name: &str,
) -> Option<ManagedSkillManifestEntry> {
    let content = std::fs::read_to_string(clawshell_config_path).ok()?;
    let json = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    let manifest = json.get(MANIFEST_CONFIG_KEY)?.as_array()?;
    for item in manifest {
        if let Ok(parsed) = serde_json::from_value::<ManagedSkillManifestEntry>(item.clone())
            && parsed.skill_name == skill_name
        {
            return Some(parsed);
        }
    }
    None
}

pub fn inspect_managed_skill_for_uninstall(
    skill_dir: &Path,
    expected_skill_name: &str,
    manifest_entry: Option<&ManagedSkillManifestEntry>,
) -> ManagedSkillInspection {
    if !skill_dir.exists() {
        return ManagedSkillInspection::missing();
    }

    let metadata_path = skill_dir.join(MANAGED_SKILL_METADATA_FILE_NAME);
    if !metadata_path.exists() {
        return as_unmanaged(format!(
            "ownership marker missing ({})",
            metadata_path.display()
        ));
    }

    let metadata_content = match std::fs::read_to_string(&metadata_path) {
        Ok(content) => content,
        Err(error) => {
            return as_unmanaged(format!(
                "failed to read ownership marker {}: {error}",
                metadata_path.display()
            ));
        }
    };

    let metadata = match serde_json::from_str::<ManagedSkillMetadata>(&metadata_content) {
        Ok(metadata) => metadata,
        Err(error) => {
            return as_unmanaged(format!(
                "ownership marker parse failed at {}: {error}",
                metadata_path.display()
            ));
        }
    };

    if metadata.managed_by != MANAGED_BY_CLAWSHELL {
        return as_unmanaged(format!(
            "ownership marker managed_by mismatch: expected '{MANAGED_BY_CLAWSHELL}', got '{}'",
            metadata.managed_by
        ));
    }
    if metadata.schema_version != MANAGED_SKILL_SCHEMA_VERSION {
        return as_unmanaged(format!(
            "ownership marker schema mismatch: expected {}, got {}",
            MANAGED_SKILL_SCHEMA_VERSION, metadata.schema_version
        ));
    }
    if metadata.skill_name != expected_skill_name {
        return as_unmanaged(format!(
            "ownership marker skill_name mismatch: expected '{expected_skill_name}', got '{}'",
            metadata.skill_name
        ));
    }

    let Some(manifest) = manifest_entry else {
        return as_unmanaged("manifest entry missing in ClawShell config");
    };
    if manifest.schema_version != metadata.schema_version {
        return as_unmanaged(format!(
            "manifest schema mismatch: marker={}, manifest={}",
            metadata.schema_version, manifest.schema_version
        ));
    }
    if manifest.skill_name != metadata.skill_name {
        return as_unmanaged(format!(
            "manifest skill_name mismatch: marker='{}', manifest='{}'",
            metadata.skill_name, manifest.skill_name
        ));
    }
    if manifest.install_id != metadata.install_id {
        return as_unmanaged(format!(
            "manifest install_id mismatch: marker='{}', manifest='{}'",
            metadata.install_id, manifest.install_id
        ));
    }

    let manifest_dir = PathBuf::from(&manifest.skill_dir);
    if canonicalize_or_original(skill_dir) != canonicalize_or_original(&manifest_dir) {
        return as_unmanaged(format!(
            "manifest path mismatch: found '{}', manifest '{}'",
            skill_dir.display(),
            manifest.skill_dir
        ));
    }

    for relative_path in &metadata.files {
        let Some(expected_hash) = metadata.hashes.get(relative_path) else {
            return as_unmanaged(format!(
                "ownership marker missing hash for managed file '{relative_path}'"
            ));
        };

        let full_path = skill_dir.join(relative_path);
        let content = match std::fs::read(&full_path) {
            Ok(content) => content,
            Err(error) => {
                return as_managed_modified(format!(
                    "managed file changed or missing '{}': {error}",
                    full_path.display()
                ));
            }
        };
        let current_hash = hash_content(&content);
        if current_hash != *expected_hash {
            return as_managed_modified(format!(
                "managed file hash changed for '{}'",
                full_path.display()
            ));
        }
    }

    ManagedSkillInspection {
        state: ManagedSkillUninstallState::ManagedUnchanged,
        detail: "ownership marker and manifest verified".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_files(skill_dir: &Path, files: &[(String, String)]) {
        for (relative_path, content) in files {
            let full_path = skill_dir.join(relative_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full_path, content).unwrap();
        }
    }

    #[test]
    fn test_inspect_managed_skill_unchanged() {
        let temp_dir = tempdir().unwrap();
        let skill_dir = temp_dir.path().join("skills/get-email-messages");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let managed_files = vec![
            ("SKILL.md".to_string(), "hello".to_string()),
            (
                "references/api-usage.md".to_string(),
                "reference".to_string(),
            ),
        ];
        write_files(&skill_dir, &managed_files);

        let metadata = build_managed_skill_metadata("get-email-messages", &managed_files);
        write_managed_skill_metadata(&skill_dir, &metadata).unwrap();
        let manifest = build_managed_skill_manifest_entry(&skill_dir, &metadata);

        let inspection =
            inspect_managed_skill_for_uninstall(&skill_dir, "get-email-messages", Some(&manifest));
        assert_eq!(
            inspection.state,
            ManagedSkillUninstallState::ManagedUnchanged
        );
    }

    #[test]
    fn test_inspect_managed_skill_modified() {
        let temp_dir = tempdir().unwrap();
        let skill_dir = temp_dir.path().join("skills/get-email-messages");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let managed_files = vec![("SKILL.md".to_string(), "before".to_string())];
        write_files(&skill_dir, &managed_files);

        let metadata = build_managed_skill_metadata("get-email-messages", &managed_files);
        write_managed_skill_metadata(&skill_dir, &metadata).unwrap();
        let manifest = build_managed_skill_manifest_entry(&skill_dir, &metadata);

        std::fs::write(skill_dir.join("SKILL.md"), "after").unwrap();

        let inspection =
            inspect_managed_skill_for_uninstall(&skill_dir, "get-email-messages", Some(&manifest));
        assert_eq!(
            inspection.state,
            ManagedSkillUninstallState::ManagedModified
        );
        assert!(inspection.detail.contains("hash changed"));
    }

    #[test]
    fn test_inspect_managed_skill_unmanaged_without_marker() {
        let temp_dir = tempdir().unwrap();
        let skill_dir = temp_dir.path().join("skills/get-email-messages");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "content").unwrap();

        let inspection =
            inspect_managed_skill_for_uninstall(&skill_dir, "get-email-messages", None);
        assert_eq!(inspection.state, ManagedSkillUninstallState::Unmanaged);
        assert!(inspection.detail.contains("ownership marker missing"));
    }

    #[test]
    fn test_inspect_managed_skill_unmanaged_on_manifest_mismatch() {
        let temp_dir = tempdir().unwrap();
        let skill_dir = temp_dir.path().join("skills/get-email-messages");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let managed_files = vec![("SKILL.md".to_string(), "hello".to_string())];
        write_files(&skill_dir, &managed_files);

        let metadata = build_managed_skill_metadata("get-email-messages", &managed_files);
        write_managed_skill_metadata(&skill_dir, &metadata).unwrap();
        let mut manifest = build_managed_skill_manifest_entry(&skill_dir, &metadata);
        manifest.install_id = Uuid::new_v4().to_string();

        let inspection =
            inspect_managed_skill_for_uninstall(&skill_dir, "get-email-messages", Some(&manifest));
        assert_eq!(inspection.state, ManagedSkillUninstallState::Unmanaged);
        assert!(inspection.detail.contains("install_id mismatch"));
    }

    #[test]
    fn test_upsert_and_read_manifest_entry() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.json");
        std::fs::write(
            &config_path,
            r#"{"real_api_key":"x","virtual_api_key":"y","provider":"openai","model":"gpt"}"#,
        )
        .unwrap();

        let entry = ManagedSkillManifestEntry {
            schema_version: 1,
            skill_name: "get-email-messages".to_string(),
            skill_dir: "/tmp/.openclaw/skills/get-email-messages".to_string(),
            install_id: Uuid::new_v4().to_string(),
        };
        upsert_managed_skill_manifest_entry(&config_path, &entry).unwrap();

        let parsed = read_managed_skill_manifest_entry(&config_path, "get-email-messages").unwrap();
        assert_eq!(parsed, entry);

        let mut replacement = parsed.clone();
        replacement.install_id = Uuid::new_v4().to_string();
        upsert_managed_skill_manifest_entry(&config_path, &replacement).unwrap();
        let updated =
            read_managed_skill_manifest_entry(&config_path, "get-email-messages").unwrap();
        assert_eq!(updated, replacement);
    }
}
