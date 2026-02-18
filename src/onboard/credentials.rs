use super::types::{
    OpenclawCredentialCleanupPreview, OpenclawCredentialCleanupSummary, OpenclawFileRemovalPreview,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use vfs::VfsPath;

const OPENCLAW_LEGACY_ENV_KEYS: [&str; 3] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
];
const OPENCLAW_LEGACY_AUTH_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "anthropic"];

/// API keys detected from an existing OpenClaw installation.
#[derive(Debug, Default)]
struct DetectedKeys {
    anthropic: Option<String>,
    openai: Option<String>,
}

impl DetectedKeys {
    /// Pick the key matching the given provider name.
    fn for_provider(&self, provider: &str) -> Option<&str> {
        match provider {
            "anthropic" => self.anthropic.as_deref(),
            "openai" => self.openai.as_deref(),
            _ => None,
        }
    }
}

pub(super) fn detect_openclaw_api_key_for_provider(provider: &str) -> Option<String> {
    detect_openclaw_api_keys()
        .for_provider(provider)
        .map(|value| value.to_string())
}

/// Detect existing API keys from an OpenClaw installation.
///
/// Searches these locations in order:
/// 1. `auth-profiles.json` files inside `<state_dir>/agents/*/agent/`
/// 2. `.env` file in `<state_dir>`
/// 3. Environment variables `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`
fn detect_openclaw_api_keys() -> DetectedKeys {
    detect_openclaw_api_keys_with_home(std::env::var("HOME").ok().as_deref())
}

/// Inner implementation that accepts an explicit home dir for testability.
fn detect_openclaw_api_keys_with_home(home: Option<&str>) -> DetectedKeys {
    let root = crate::process::physical_root();
    match home {
        Some(h) => match root.join(h.trim_start_matches('/')) {
            Ok(home_vfs) => detect_openclaw_api_keys_vfs(&home_vfs),
            Err(_) => DetectedKeys {
                anthropic: std::env::var("ANTHROPIC_API_KEY").ok(),
                openai: std::env::var("OPENAI_API_KEY").ok(),
            },
        },
        None => DetectedKeys {
            anthropic: std::env::var("ANTHROPIC_API_KEY").ok(),
            openai: std::env::var("OPENAI_API_KEY").ok(),
        },
    }
}

/// VFS implementation of API key detection from filesystem sources.
/// Falls back to environment variables for any keys not found on the filesystem.
fn detect_openclaw_api_keys_vfs(home: &VfsPath) -> DetectedKeys {
    let mut keys = DetectedKeys::default();

    // Find the state directory
    let state_dir = match find_state_dir_vfs(home) {
        Some(d) => d,
        None => {
            // Fall back to env vars only
            keys.anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
            keys.openai = std::env::var("OPENAI_API_KEY").ok();
            return keys;
        }
    };

    // Strategy 1: auth-profiles.json
    try_auth_profiles_vfs(&state_dir, &mut keys);

    // Strategy 2: .env file
    if keys.anthropic.is_none() || keys.openai.is_none() {
        try_dot_env_vfs(&state_dir, &mut keys);
    }

    // Strategy 3: environment variables
    if keys.anthropic.is_none() {
        keys.anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
    }
    if keys.openai.is_none() {
        keys.openai = std::env::var("OPENAI_API_KEY").ok();
    }

    keys
}

/// Find the first existing OpenClaw state directory (VFS variant).
fn find_state_dir_vfs(home: &VfsPath) -> Option<VfsPath> {
    let candidates = [".openclaw", ".clawdbot", ".moltbot", ".moldbot"];
    for name in &candidates {
        if let Ok(path) = home.join(name)
            && path.exists().unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

/// Scan auth-profiles.json files for API keys (VFS variant).
fn try_auth_profiles_vfs(state_dir: &VfsPath, keys: &mut DetectedKeys) {
    let agents_dir = match state_dir.join("agents") {
        Ok(d) => d,
        Err(_) => return,
    };
    let entries = match agents_dir.read_dir() {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        let profile_path = match entry.join("agent/auth-profiles.json") {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Ok(content) = profile_path.read_to_string()
            && let Ok(json) = serde_json::from_str::<Value>(&content)
            && let Some(profiles) = json.get("profiles").and_then(|p| p.as_object())
        {
            if keys.anthropic.is_none()
                && let Some(key) = profiles
                    .get("anthropic:default")
                    .and_then(|p| p.get("key"))
                    .and_then(|k| k.as_str())
                && !key.is_empty()
            {
                keys.anthropic = Some(key.to_string());
            }
            if keys.openai.is_none()
                && let Some(key) = profiles
                    .get("openai:default")
                    .and_then(|p| p.get("key"))
                    .and_then(|k| k.as_str())
                && !key.is_empty()
            {
                keys.openai = Some(key.to_string());
            }
        }
        if keys.anthropic.is_some() && keys.openai.is_some() {
            break;
        }
    }
}

/// Parse a .env file for API keys (VFS variant).
fn try_dot_env_vfs(state_dir: &VfsPath, keys: &mut DetectedKeys) {
    let env_path = match state_dir.join(".env") {
        Ok(p) => p,
        Err(_) => return,
    };
    let content = match env_path.read_to_string() {
        Ok(c) => c,
        Err(_) => return,
    };
    parse_dot_env_content(&content, keys);
}

/// Shared .env parsing logic.
fn parse_dot_env_content(content: &str, keys: &mut DetectedKeys) {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if v.is_empty() {
                continue;
            }
            if k == "ANTHROPIC_API_KEY" && keys.anthropic.is_none() {
                keys.anthropic = Some(v.to_string());
            } else if k == "OPENAI_API_KEY" && keys.openai.is_none() {
                keys.openai = Some(v.to_string());
            }
        }
    }
}

fn is_legacy_env_key(key: &str) -> bool {
    OPENCLAW_LEGACY_ENV_KEYS.contains(&key)
}

fn is_legacy_provider_name(provider: &str) -> bool {
    OPENCLAW_LEGACY_AUTH_PROVIDERS.contains(&provider)
}

fn is_legacy_provider_entry(value: &str) -> bool {
    let provider = value.trim().split(':').next().unwrap_or(value.trim());
    is_legacy_provider_name(provider)
}

fn parse_dot_env_assignment(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let key_value = if let Some(rest) = trimmed.strip_prefix("export") {
        let trimmed_rest = rest.trim_start();
        if trimmed_rest.len() != rest.len() {
            trimmed_rest
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    let (key, value) = key_value.split_once('=')?;
    Some((
        key.trim().to_string(),
        value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string(),
    ))
}

fn should_remove_dot_env_line(line: &str, mapped_real_key: &str) -> bool {
    let Some((key, value)) = parse_dot_env_assignment(line) else {
        return false;
    };
    is_legacy_env_key(&key) && value == mapped_real_key
}

fn remove_legacy_entries_from_dot_env(content: &str, mapped_real_key: &str) -> (String, usize) {
    let mut kept = Vec::new();
    let mut removed = 0usize;

    for line in content.lines() {
        if should_remove_dot_env_line(line, mapped_real_key) {
            removed += 1;
        } else {
            kept.push(line);
        }
    }

    let mut updated = kept.join("\n");
    if content.ends_with('\n') && !updated.ends_with('\n') {
        updated.push('\n');
    }

    (updated, removed)
}

fn preview_legacy_dot_env_removals(content: &str, mapped_real_key: &str) -> Vec<String> {
    content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            if !should_remove_dot_env_line(line, mapped_real_key) {
                return None;
            }
            let key = parse_dot_env_assignment(line)
                .map(|(key, _)| key)
                .unwrap_or_else(|| "<unknown>".to_string());
            Some(format!("line {}: {}", index + 1, key))
        })
        .collect()
}

fn next_pre_edit_backup_path_vfs(
    file_path: &VfsPath,
) -> Result<VfsPath, Box<dyn std::error::Error>> {
    let parent = file_path.parent();
    let file_name = file_path
        .as_str()
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .ok_or_else(|| format!("invalid path for backup: {}", file_path.as_str()))?;
    let base_backup = parent.join(format!("{file_name}.clawshell.bak"))?;
    if !base_backup.exists()? {
        return Ok(base_backup);
    }

    let mut n = 1usize;
    loop {
        let candidate = parent.join(format!("{file_name}.clawshell.bak.{n}"))?;
        if !candidate.exists()? {
            return Ok(candidate);
        }
        n += 1;
    }
}

fn backup_file_before_edit_vfs(file_path: &VfsPath) -> Result<VfsPath, Box<dyn std::error::Error>> {
    if !file_path.exists()? {
        return Err(format!("cannot back up missing file: {}", file_path.as_str()).into());
    }

    let backup_path = next_pre_edit_backup_path_vfs(file_path)?;
    let content = file_path.read_to_string()?;
    backup_path.create_file()?.write_all(content.as_bytes())?;
    Ok(backup_path)
}

fn profile_entry_matches_mapped_key(
    profile_id: &str,
    value: &Value,
    mapped_real_key: &str,
) -> bool {
    if !is_legacy_provider_entry(profile_id) {
        return false;
    }

    match value {
        Value::Object(map) => map
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|v| v == mapped_real_key),
        Value::String(v) => v == mapped_real_key,
        _ => false,
    }
}

fn collect_profile_ids_to_remove(
    profiles: &serde_json::Map<String, Value>,
    mapped_real_key: &str,
) -> std::collections::HashSet<String> {
    profiles
        .iter()
        .filter_map(|(profile_id, value)| {
            if profile_entry_matches_mapped_key(profile_id, value, mapped_real_key) {
                Some(profile_id.clone())
            } else {
                None
            }
        })
        .collect()
}

fn collect_sorted_profile_ids_to_remove(
    profiles: &serde_json::Map<String, Value>,
    mapped_real_key: &str,
) -> Vec<String> {
    let mut sorted: Vec<String> = collect_profile_ids_to_remove(profiles, mapped_real_key)
        .into_iter()
        .collect();
    sorted.sort_unstable();
    sorted
}

fn value_references_removed_profile_id(
    value: &Value,
    removed_profile_ids: &std::collections::HashSet<String>,
) -> bool {
    match value {
        Value::String(v) => removed_profile_ids.contains(v),
        Value::Object(map) => {
            map.get("profile")
                .and_then(Value::as_str)
                .is_some_and(|v| removed_profile_ids.contains(v))
                || map
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|v| removed_profile_ids.contains(v))
        }
        _ => false,
    }
}

fn remove_profile_references_from_value(
    value: &mut Value,
    removed_profile_ids: &std::collections::HashSet<String>,
) -> usize {
    match value {
        Value::Array(items) => {
            let before = items.len();
            items.retain(|item| !value_references_removed_profile_id(item, removed_profile_ids));
            before - items.len()
        }
        Value::Object(map) => {
            let keys_to_remove: Vec<String> = map
                .iter()
                .filter_map(|(key, value)| {
                    if removed_profile_ids.contains(key)
                        || value_references_removed_profile_id(value, removed_profile_ids)
                    {
                        Some(key.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let removed = keys_to_remove.len();
            for key in keys_to_remove {
                map.remove(&key);
            }
            removed
        }
        Value::String(v) => {
            if removed_profile_ids.contains(v) {
                *value = Value::Null;
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn collect_profile_reference_removals(
    value: &Value,
    removed_profile_ids: &std::collections::HashSet<String>,
    field_name: &str,
) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                if !value_references_removed_profile_id(item, removed_profile_ids) {
                    return None;
                }
                match item {
                    Value::String(v) => Some(format!("{field_name}[{index}] => {v}")),
                    Value::Object(map) => {
                        let profile = map.get("profile").and_then(Value::as_str);
                        let id = map.get("id").and_then(Value::as_str);
                        let ref_value = profile.or(id).unwrap_or("<unknown>");
                        Some(format!("{field_name}[{index}] => {ref_value}"))
                    }
                    _ => Some(format!("{field_name}[{index}]")),
                }
            })
            .collect(),
        Value::Object(map) => map
            .iter()
            .filter_map(|(key, value)| {
                if removed_profile_ids.contains(key) {
                    return Some(format!("{field_name}.{key}"));
                }
                if !value_references_removed_profile_id(value, removed_profile_ids) {
                    return None;
                }
                match value {
                    Value::String(v) => Some(format!("{field_name}.{key} => {v}")),
                    Value::Object(fields) => {
                        let profile = fields.get("profile").and_then(Value::as_str);
                        let id = fields.get("id").and_then(Value::as_str);
                        let ref_value = profile.or(id).unwrap_or("<unknown>");
                        Some(format!("{field_name}.{key} => {ref_value}"))
                    }
                    _ => Some(format!("{field_name}.{key}")),
                }
            })
            .collect(),
        Value::String(v) => {
            if removed_profile_ids.contains(v) {
                vec![format!("{field_name} => {v}")]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn collect_auth_profile_removals(
    content: &str,
    mapped_real_key: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let json: Value = serde_json::from_str(content)?;
    let mut removals = Vec::new();

    let Some(root) = json.as_object() else {
        return Ok(removals);
    };

    let mut removed_profile_ids = std::collections::HashSet::new();
    if let Some(profiles) = root.get("profiles").and_then(Value::as_object) {
        for profile_id in collect_sorted_profile_ids_to_remove(profiles, mapped_real_key) {
            removals.push(format!("profiles.{profile_id}"));
            removed_profile_ids.insert(profile_id);
        }
    }

    if removed_profile_ids.is_empty() {
        return Ok(removals);
    }

    if let Some(order) = root.get("order") {
        removals.extend(collect_profile_reference_removals(
            order,
            &removed_profile_ids,
            "order",
        ));
    }
    if let Some(last_good) = root.get("lastGood") {
        removals.extend(collect_profile_reference_removals(
            last_good,
            &removed_profile_ids,
            "lastGood",
        ));
    }

    Ok(removals)
}

fn oauth_entry_matches_mapped_key(value: &Value, mapped_real_key: &str) -> bool {
    match value {
        Value::String(v) => v == mapped_real_key,
        Value::Object(fields) => {
            fields
                .get("token")
                .and_then(Value::as_str)
                .is_some_and(|v| v == mapped_real_key)
                || fields
                    .get("access_token")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v == mapped_real_key)
                || fields
                    .get("api_key")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v == mapped_real_key)
                || fields
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v == mapped_real_key)
        }
        _ => false,
    }
}

fn collect_oauth_provider_keys_to_remove(
    map: &serde_json::Map<String, Value>,
    mapped_real_key: &str,
) -> Vec<String> {
    let mut providers_to_remove: Vec<String> = map
        .iter()
        .filter_map(|(provider, value)| {
            if !is_legacy_provider_name(provider) {
                return None;
            }
            if oauth_entry_matches_mapped_key(value, mapped_real_key) {
                Some(provider.clone())
            } else {
                None
            }
        })
        .collect();
    providers_to_remove.sort_unstable();
    providers_to_remove
}

fn remove_legacy_auth_profile_entries(
    content: &str,
    mapped_real_key: &str,
) -> Result<(String, usize), Box<dyn std::error::Error>> {
    let mut json: Value = serde_json::from_str(content)?;
    let mut removed_entries = 0usize;
    let mut removed_profile_ids = std::collections::HashSet::new();

    if let Some(profiles) = json.get_mut("profiles").and_then(Value::as_object_mut) {
        removed_profile_ids = collect_profile_ids_to_remove(profiles, mapped_real_key);
        removed_entries += removed_profile_ids.len();
        for profile_id in &removed_profile_ids {
            profiles.remove(profile_id);
        }
    }

    if !removed_profile_ids.is_empty()
        && let Some(root) = json.as_object_mut()
    {
        let mut remove_order_field = false;
        if let Some(order) = root.get_mut("order") {
            removed_entries += remove_profile_references_from_value(order, &removed_profile_ids);
            if order.is_null() {
                remove_order_field = true;
            }
        }
        if remove_order_field {
            root.remove("order");
        }

        let mut remove_last_good_field = false;
        if let Some(last_good) = root.get_mut("lastGood") {
            removed_entries +=
                remove_profile_references_from_value(last_good, &removed_profile_ids);
            if last_good.is_null() {
                remove_last_good_field = true;
            }
        }
        if remove_last_good_field {
            root.remove("lastGood");
        }
    }

    if removed_entries == 0 {
        return Ok((content.to_string(), 0));
    }

    let mut updated = serde_json::to_string_pretty(&json)?;
    updated.push('\n');
    Ok((updated, removed_entries))
}

fn preview_state_dir_dot_env_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<Option<OpenclawFileRemovalPreview>, Box<dyn std::error::Error>> {
    let env_path = match state_dir.join(".env") {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !env_path.exists()? {
        return Ok(None);
    }

    let content = env_path.read_to_string()?;
    let removals = preview_legacy_dot_env_removals(&content, mapped_real_key);
    if removals.is_empty() {
        return Ok(None);
    }

    let backup_path = next_pre_edit_backup_path_vfs(&env_path)?;
    Ok(Some(OpenclawFileRemovalPreview {
        path: PathBuf::from(env_path.as_str()),
        backup_path: PathBuf::from(backup_path.as_str()),
        removals,
    }))
}

fn preview_auth_profiles_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<Vec<OpenclawFileRemovalPreview>, Box<dyn std::error::Error>> {
    let agents_dir = match state_dir.join("agents") {
        Ok(path) => path,
        Err(_) => return Ok(Vec::new()),
    };
    if !agents_dir.exists()? {
        return Ok(Vec::new());
    }

    let entries = match agents_dir.read_dir() {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };

    let mut previews = Vec::new();
    for entry in entries {
        let profile_path = match entry.join("agent/auth-profiles.json") {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !profile_path.exists()? {
            continue;
        }

        let content = profile_path.read_to_string()?;
        let removals = collect_auth_profile_removals(&content, mapped_real_key)?;
        if removals.is_empty() {
            continue;
        }

        let backup_path = next_pre_edit_backup_path_vfs(&profile_path)?;
        previews.push(OpenclawFileRemovalPreview {
            path: PathBuf::from(profile_path.as_str()),
            backup_path: PathBuf::from(backup_path.as_str()),
            removals,
        });
    }

    previews.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(previews)
}

fn preview_legacy_oauth_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<Option<OpenclawFileRemovalPreview>, Box<dyn std::error::Error>> {
    let oauth_path = match state_dir.join("credentials/oauth.json") {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !oauth_path.exists()? {
        return Ok(None);
    }

    let content = oauth_path.read_to_string()?;
    let json: Value = serde_json::from_str(&content)?;

    let removals = json
        .as_object()
        .map(|map| collect_oauth_provider_keys_to_remove(map, mapped_real_key))
        .unwrap_or_default()
        .into_iter()
        .map(|provider| format!("oauth.{provider}"))
        .collect::<Vec<String>>();

    if removals.is_empty() {
        return Ok(None);
    }

    let backup_path = next_pre_edit_backup_path_vfs(&oauth_path)?;
    Ok(Some(OpenclawFileRemovalPreview {
        path: PathBuf::from(oauth_path.as_str()),
        backup_path: PathBuf::from(backup_path.as_str()),
        removals,
    }))
}

fn preview_openclaw_provider_credentials_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<OpenclawCredentialCleanupPreview, Box<dyn std::error::Error>> {
    let state_dir_exists = state_dir.exists()?;
    if !state_dir_exists {
        return Ok(OpenclawCredentialCleanupPreview {
            state_dir: PathBuf::from(state_dir.as_str()),
            state_dir_exists: false,
            ..OpenclawCredentialCleanupPreview::default()
        });
    }

    Ok(OpenclawCredentialCleanupPreview {
        state_dir: PathBuf::from(state_dir.as_str()),
        state_dir_exists: true,
        dot_env: preview_state_dir_dot_env_vfs(state_dir, mapped_real_key)?,
        auth_profiles: preview_auth_profiles_vfs(state_dir, mapped_real_key)?,
        oauth: preview_legacy_oauth_vfs(state_dir, mapped_real_key)?,
    })
}

fn cleanup_state_dir_dot_env_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let env_path = match state_dir.join(".env") {
        Ok(path) => path,
        Err(_) => return Ok((0, 0)),
    };
    if !env_path.exists()? {
        return Ok((0, 0));
    }

    let content = env_path.read_to_string()?;
    let (updated, removed) = remove_legacy_entries_from_dot_env(&content, mapped_real_key);
    let mut backup_files_created = 0usize;
    if removed > 0 {
        backup_file_before_edit_vfs(&env_path)?;
        backup_files_created += 1;
        env_path.create_file()?.write_all(updated.as_bytes())?;
    }
    Ok((removed, backup_files_created))
}

fn cleanup_auth_profiles_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<(usize, usize, usize), Box<dyn std::error::Error>> {
    let agents_dir = match state_dir.join("agents") {
        Ok(path) => path,
        Err(_) => return Ok((0, 0, 0)),
    };
    if !agents_dir.exists()? {
        return Ok((0, 0, 0));
    }

    let entries = match agents_dir.read_dir() {
        Ok(entries) => entries,
        Err(_) => return Ok((0, 0, 0)),
    };

    let mut files_updated = 0usize;
    let mut entries_removed = 0usize;
    let mut backup_files_created = 0usize;

    for entry in entries {
        let profile_path = match entry.join("agent/auth-profiles.json") {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !profile_path.exists()? {
            continue;
        }

        let content = profile_path.read_to_string()?;
        let (updated, removed) = remove_legacy_auth_profile_entries(&content, mapped_real_key)?;
        if removed > 0 {
            backup_file_before_edit_vfs(&profile_path)?;
            backup_files_created += 1;
            profile_path.create_file()?.write_all(updated.as_bytes())?;
            files_updated += 1;
            entries_removed += removed;
        }
    }

    Ok((files_updated, entries_removed, backup_files_created))
}

fn cleanup_legacy_oauth_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let oauth_path = match state_dir.join("credentials/oauth.json") {
        Ok(path) => path,
        Err(_) => return Ok((0, 0)),
    };
    if !oauth_path.exists()? {
        return Ok((0, 0));
    }

    let content = oauth_path.read_to_string()?;
    let mut json: Value = serde_json::from_str(&content)?;

    let mut removed = 0usize;
    if let Some(map) = json.as_object_mut() {
        let providers_to_remove = collect_oauth_provider_keys_to_remove(map, mapped_real_key);

        removed += providers_to_remove.len();
        for provider in providers_to_remove {
            map.remove(&provider);
        }
    }

    let mut backup_files_created = 0usize;
    if removed > 0 {
        backup_file_before_edit_vfs(&oauth_path)?;
        backup_files_created += 1;
        let mut updated = serde_json::to_string_pretty(&json)?;
        updated.push('\n');
        oauth_path.create_file()?.write_all(updated.as_bytes())?;
    }

    Ok((removed, backup_files_created))
}

fn cleanup_openclaw_provider_credentials_vfs(
    state_dir: &VfsPath,
    mapped_real_key: &str,
) -> Result<OpenclawCredentialCleanupSummary, Box<dyn std::error::Error>> {
    if !state_dir.exists()? {
        return Ok(OpenclawCredentialCleanupSummary::default());
    }

    let (dot_env_entries_removed, dot_env_backup_files_created) =
        cleanup_state_dir_dot_env_vfs(state_dir, mapped_real_key)?;
    let (
        auth_profile_files_updated,
        auth_profile_entries_removed,
        auth_profile_backup_files_created,
    ) = cleanup_auth_profiles_vfs(state_dir, mapped_real_key)?;
    let (oauth_entries_removed, oauth_backup_files_created) =
        cleanup_legacy_oauth_vfs(state_dir, mapped_real_key)?;
    let backup_files_created = dot_env_backup_files_created
        + auth_profile_backup_files_created
        + oauth_backup_files_created;

    Ok(OpenclawCredentialCleanupSummary {
        dot_env_entries_removed,
        auth_profile_files_updated,
        auth_profile_entries_removed,
        oauth_entries_removed,
        backup_files_created,
    })
}

/// Remove legacy OpenClaw provider credentials that match the real key mapped
/// by the ClawShell virtual key.
///
/// This cleans:
/// - `<state_dir>/.env`
/// - `<state_dir>/agents/*/agent/auth-profiles.json`
/// - `<state_dir>/credentials/oauth.json`
///
/// Before each edited file is written, a sibling backup is created with suffix
/// `.clawshell.bak` (or `.clawshell.bak.<n>` if needed).
pub fn cleanup_openclaw_provider_credentials(
    state_dir: &Path,
    mapped_real_key: &str,
) -> Result<OpenclawCredentialCleanupSummary, Box<dyn std::error::Error>> {
    let normalized = if state_dir.is_absolute() {
        state_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(state_dir)
    };

    let root = crate::process::physical_root();
    let vfs_path = root.join(normalized.to_string_lossy().trim_start_matches('/'))?;
    cleanup_openclaw_provider_credentials_vfs(&vfs_path, mapped_real_key)
}

/// Preview legacy OpenClaw provider credentials that would be removed for the
/// mapped real key, including concrete file paths and backup targets.
///
/// This function is non-mutating and is intended for pre-approval UX.
pub fn preview_openclaw_provider_credential_cleanup(
    state_dir: &Path,
    mapped_real_key: &str,
) -> Result<OpenclawCredentialCleanupPreview, Box<dyn std::error::Error>> {
    let normalized = if state_dir.is_absolute() {
        state_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(state_dir)
    };

    let root = crate::process::physical_root();
    let vfs_path = root.join(normalized.to_string_lossy().trim_start_matches('/'))?;
    preview_openclaw_provider_credentials_vfs(&vfs_path, mapped_real_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::test_support::vfs_write;
    use vfs::MemoryFS;

    #[test]
    fn test_detect_keys_from_auth_profiles() {
        let root = VfsPath::new(MemoryFS::new());
        let profiles = serde_json::json!({
            "profiles": {
                "anthropic:default": { "key": "sk-ant-detect-123" },
                "openai:default": { "key": "sk-oai-detect-456" }
            }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/agents/myagent/agent/auth-profiles.json",
            &serde_json::to_string(&profiles).unwrap(),
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-detect-123"));
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-detect-456"));
    }

    #[test]
    fn test_detect_keys_from_dot_env() {
        let root = VfsPath::new(MemoryFS::new());
        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "ANTHROPIC_API_KEY=sk-ant-env-789\nOPENAI_API_KEY=sk-oai-env-012\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-env-789"));
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-env-012"));
    }

    #[test]
    fn test_cleanup_openclaw_provider_credentials_vfs_removes_legacy_entries() {
        let root = VfsPath::new(MemoryFS::new());
        let mapped_real_key = "sk-oai-profile";

        let dot_env_original = "OPENAI_API_KEY=sk-oai-env\n\
             export ANTHROPIC_API_KEY=sk-ant-env\n\
             ANTHROPIC_OAUTH_TOKEN=ant-oauth\n\
             KEEP_ME=1\n";
        let dot_env_original = dot_env_original.replace("sk-oai-env", mapped_real_key);
        vfs_write(&root, "home/user/.openclaw/.env", dot_env_original.as_str());

        let profiles = serde_json::json!({
            "profiles": {
                "openai:default": { "key": mapped_real_key },
                "openai-codex:default": { "key": "sk-oai-codex-profile" },
                "anthropic:default": { "key": "sk-ant-profile" },
                "custom:default": { "key": "sk-custom-profile" }
            },
            "order": [
                "openai:default",
                "openai-codex:default",
                "anthropic:default",
                "custom:default"
            ],
            "lastGood": {
                "openai": "openai:default",
                "openai-codex": "openai-codex:default",
                "anthropic": "anthropic:default",
                "custom": "custom:default"
            }
        });
        let profiles_a1_original = serde_json::to_string_pretty(&profiles).unwrap();
        vfs_write(
            &root,
            "home/user/.openclaw/agents/a1/agent/auth-profiles.json",
            &profiles_a1_original,
        );

        let untouched_profiles = serde_json::json!({
            "profiles": {
                "custom:default": { "key": "only-custom" }
            },
            "order": ["custom:default"],
            "lastGood": {
                "custom": "custom:default"
            }
        });
        let profiles_a2_original = serde_json::to_string_pretty(&untouched_profiles).unwrap();
        vfs_write(
            &root,
            "home/user/.openclaw/agents/a2/agent/auth-profiles.json",
            &profiles_a2_original,
        );

        let oauth = serde_json::json!({
            "openai": { "token": mapped_real_key },
            "openai-codex": { "token": "openai-codex-token" },
            "anthropic": { "token": "anthropic-token" },
            "custom": { "token": "custom-token" }
        });
        let oauth_original = serde_json::to_string_pretty(&oauth).unwrap();
        vfs_write(
            &root,
            "home/user/.openclaw/credentials/oauth.json",
            &oauth_original,
        );

        let state_dir = root.join("home/user/.openclaw").unwrap();
        let summary =
            cleanup_openclaw_provider_credentials_vfs(&state_dir, mapped_real_key).unwrap();
        assert_eq!(summary.dot_env_entries_removed, 1);
        assert_eq!(summary.auth_profile_files_updated, 1);
        assert_eq!(summary.auth_profile_entries_removed, 3);
        assert_eq!(summary.oauth_entries_removed, 1);
        assert_eq!(summary.backup_files_created, 3);
        assert!(summary.has_changes());

        let env_content = state_dir.join(".env").unwrap().read_to_string().unwrap();
        assert!(env_content.contains("KEEP_ME=1"));
        assert!(!env_content.contains("OPENAI_API_KEY"));
        assert!(env_content.contains("ANTHROPIC_API_KEY"));
        assert!(env_content.contains("ANTHROPIC_OAUTH_TOKEN"));
        let env_backup = state_dir
            .join(".env.clawshell.bak")
            .unwrap()
            .read_to_string()
            .unwrap();
        assert_eq!(env_backup, dot_env_original);

        let cleaned_profiles_content = state_dir
            .join("agents/a1/agent/auth-profiles.json")
            .unwrap()
            .read_to_string()
            .unwrap();
        let cleaned_profiles: Value = serde_json::from_str(&cleaned_profiles_content).unwrap();
        let profiles = cleaned_profiles["profiles"].as_object().unwrap();
        assert_eq!(profiles.len(), 3);
        assert!(profiles.contains_key("openai-codex:default"));
        assert!(profiles.contains_key("anthropic:default"));
        assert!(profiles.contains_key("custom:default"));
        assert!(!profiles.contains_key("openai:default"));

        let order = cleaned_profiles["order"].as_array().unwrap();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0], "openai-codex:default");
        assert_eq!(order[1], "anthropic:default");
        assert_eq!(order[2], "custom:default");

        let last_good = cleaned_profiles["lastGood"].as_object().unwrap();
        assert_eq!(last_good.len(), 3);
        assert!(last_good.contains_key("openai-codex"));
        assert!(last_good.contains_key("anthropic"));
        assert!(last_good.contains_key("custom"));
        assert!(!last_good.contains_key("openai"));
        let profiles_a1_backup = state_dir
            .join("agents/a1/agent/auth-profiles.json.clawshell.bak")
            .unwrap()
            .read_to_string()
            .unwrap();
        assert_eq!(profiles_a1_backup, profiles_a1_original);

        let untouched_after_content = state_dir
            .join("agents/a2/agent/auth-profiles.json")
            .unwrap()
            .read_to_string()
            .unwrap();
        let untouched_after: Value = serde_json::from_str(&untouched_after_content).unwrap();
        let untouched_profiles = untouched_after["profiles"].as_object().unwrap();
        assert_eq!(untouched_profiles.len(), 1);
        assert!(untouched_profiles.contains_key("custom:default"));
        let untouched_backup_path = state_dir
            .join("agents/a2/agent/auth-profiles.json.clawshell.bak")
            .unwrap();
        assert!(!untouched_backup_path.exists().unwrap());

        let oauth_content = state_dir
            .join("credentials/oauth.json")
            .unwrap()
            .read_to_string()
            .unwrap();
        let cleaned_oauth: Value = serde_json::from_str(&oauth_content).unwrap();
        let oauth = cleaned_oauth.as_object().unwrap();
        assert_eq!(oauth.len(), 3);
        assert!(oauth.contains_key("openai-codex"));
        assert!(oauth.contains_key("anthropic"));
        assert!(oauth.contains_key("custom"));
        assert!(!oauth.contains_key("openai"));
        let oauth_backup = state_dir
            .join("credentials/oauth.json.clawshell.bak")
            .unwrap()
            .read_to_string()
            .unwrap();
        assert_eq!(oauth_backup, oauth_original);
    }

    #[test]
    fn test_preview_openclaw_provider_credentials_vfs_reports_exact_edits() {
        let root = VfsPath::new(MemoryFS::new());
        let mapped_real_key = "sk-oai-preview";

        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "OPENAI_API_KEY=sk-oai-preview\nANTHROPIC_API_KEY=keep\nKEEP=1\n",
        );
        vfs_write(
            &root,
            "home/user/.openclaw/.env.clawshell.bak",
            "old backup",
        );

        let profiles = serde_json::json!({
            "profiles": {
                "openai:default": { "key": mapped_real_key },
                "custom:default": { "key": "keep" }
            },
            "order": ["openai:default", "custom:default"],
            "lastGood": {
                "openai": "openai:default",
                "custom": "custom:default"
            }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/agents/a1/agent/auth-profiles.json",
            &serde_json::to_string_pretty(&profiles).unwrap(),
        );

        let oauth = serde_json::json!({
            "openai": { "token": mapped_real_key },
            "custom": { "token": "keep" }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/credentials/oauth.json",
            &serde_json::to_string_pretty(&oauth).unwrap(),
        );
        vfs_write(
            &root,
            "home/user/.openclaw/credentials/oauth.json.clawshell.bak",
            "old oauth backup",
        );

        let state_dir = root.join("home/user/.openclaw").unwrap();
        let preview =
            preview_openclaw_provider_credentials_vfs(&state_dir, mapped_real_key).unwrap();

        assert!(preview.state_dir_exists);
        assert!(preview.has_changes());
        let files_to_edit = usize::from(preview.dot_env.is_some())
            + preview.auth_profiles.len()
            + usize::from(preview.oauth.is_some());
        assert_eq!(files_to_edit, 3);
        let total_removals = preview.dot_env.as_ref().map_or(0, |p| p.removals.len())
            + preview
                .auth_profiles
                .iter()
                .map(|p| p.removals.len())
                .sum::<usize>()
            + preview.oauth.as_ref().map_or(0, |p| p.removals.len());
        assert_eq!(total_removals, 5);

        let dot_env = preview.dot_env.as_ref().unwrap();
        assert_eq!(dot_env.path, PathBuf::from("/home/user/.openclaw/.env"));
        assert_eq!(
            dot_env.backup_path,
            PathBuf::from("/home/user/.openclaw/.env.clawshell.bak.1")
        );
        assert_eq!(dot_env.removals, vec!["line 1: OPENAI_API_KEY"]);

        assert_eq!(preview.auth_profiles.len(), 1);
        let profile = &preview.auth_profiles[0];
        assert_eq!(
            profile.path,
            PathBuf::from("/home/user/.openclaw/agents/a1/agent/auth-profiles.json")
        );
        assert_eq!(
            profile.backup_path,
            PathBuf::from("/home/user/.openclaw/agents/a1/agent/auth-profiles.json.clawshell.bak")
        );
        assert!(
            profile
                .removals
                .contains(&"profiles.openai:default".to_string())
        );
        assert!(
            profile
                .removals
                .contains(&"order[0] => openai:default".to_string())
        );
        assert!(
            profile
                .removals
                .contains(&"lastGood.openai => openai:default".to_string())
        );

        let oauth = preview.oauth.as_ref().unwrap();
        assert_eq!(
            oauth.path,
            PathBuf::from("/home/user/.openclaw/credentials/oauth.json")
        );
        assert_eq!(
            oauth.backup_path,
            PathBuf::from("/home/user/.openclaw/credentials/oauth.json.clawshell.bak.1")
        );
        assert_eq!(oauth.removals, vec!["oauth.openai"]);
    }

    #[test]
    fn test_preview_openclaw_provider_credentials_vfs_no_matches() {
        let root = VfsPath::new(MemoryFS::new());
        let mapped_real_key = "sk-oai-preview";

        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "OPENAI_API_KEY=other\nANTHROPIC_API_KEY=other\n",
        );
        let profiles = serde_json::json!({
            "profiles": {
                "openai:default": { "key": "different" }
            }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/agents/a1/agent/auth-profiles.json",
            &serde_json::to_string_pretty(&profiles).unwrap(),
        );
        let oauth = serde_json::json!({
            "openai": { "token": "different" }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/credentials/oauth.json",
            &serde_json::to_string_pretty(&oauth).unwrap(),
        );

        let state_dir = root.join("home/user/.openclaw").unwrap();
        let preview =
            preview_openclaw_provider_credentials_vfs(&state_dir, mapped_real_key).unwrap();

        assert!(preview.state_dir_exists);
        assert!(!preview.has_changes());
        let files_to_edit = usize::from(preview.dot_env.is_some())
            + preview.auth_profiles.len()
            + usize::from(preview.oauth.is_some());
        assert_eq!(files_to_edit, 0);
        let total_removals = preview.dot_env.as_ref().map_or(0, |p| p.removals.len())
            + preview
                .auth_profiles
                .iter()
                .map(|p| p.removals.len())
                .sum::<usize>()
            + preview.oauth.as_ref().map_or(0, |p| p.removals.len());
        assert_eq!(total_removals, 0);
        assert!(preview.dot_env.is_none());
        assert!(preview.auth_profiles.is_empty());
        assert!(preview.oauth.is_none());
    }

    #[test]
    fn test_preview_openclaw_provider_credentials_vfs_missing_state_dir() {
        let root = VfsPath::new(MemoryFS::new());
        root.join("home/user").unwrap().create_dir_all().unwrap();

        let state_dir = root.join("home/user/.openclaw").unwrap();
        let preview = preview_openclaw_provider_credentials_vfs(&state_dir, "sk-mapped").unwrap();

        assert!(!preview.state_dir_exists);
        assert!(!preview.has_changes());
        let files_to_edit = usize::from(preview.dot_env.is_some())
            + preview.auth_profiles.len()
            + usize::from(preview.oauth.is_some());
        assert_eq!(files_to_edit, 0);
        let total_removals = preview.dot_env.as_ref().map_or(0, |p| p.removals.len())
            + preview
                .auth_profiles
                .iter()
                .map(|p| p.removals.len())
                .sum::<usize>()
            + preview.oauth.as_ref().map_or(0, |p| p.removals.len());
        assert_eq!(total_removals, 0);
    }

    #[test]
    fn test_detect_keys_auth_profiles_takes_priority_over_dot_env() {
        let root = VfsPath::new(MemoryFS::new());

        // auth-profiles has only anthropic
        let profiles = serde_json::json!({
            "profiles": {
                "anthropic:default": { "key": "sk-ant-from-profile" }
            }
        });
        vfs_write(
            &root,
            "home/user/.openclaw/agents/a1/agent/auth-profiles.json",
            &serde_json::to_string(&profiles).unwrap(),
        );

        // .env has both
        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "ANTHROPIC_API_KEY=sk-ant-from-env\nOPENAI_API_KEY=sk-oai-from-env\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        // anthropic from auth-profiles wins
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-from-profile"));
        // openai falls through to .env
        assert_eq!(keys.openai.as_deref(), Some("sk-oai-from-env"));
    }

    #[test]
    fn test_detect_keys_no_state_dir() {
        let root = VfsPath::new(MemoryFS::new());
        // Create a home dir with no .openclaw etc.
        root.join("home/user").unwrap().create_dir_all().unwrap();

        let home = root.join("home/user").unwrap();
        // Should not panic — keys come from env vars (or be None)
        let keys = detect_openclaw_api_keys_vfs(&home);
        let _ = keys;
    }

    #[test]
    fn test_detect_keys_fallback_state_dirs() {
        let root = VfsPath::new(MemoryFS::new());

        // Only .clawdbot exists (second candidate)
        vfs_write(
            &root,
            "home/user/.clawdbot/.env",
            "ANTHROPIC_API_KEY=sk-ant-clawdbot\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-ant-clawdbot"));
    }

    #[test]
    fn test_detect_keys_dot_env_skips_empty_and_comments() {
        let root = VfsPath::new(MemoryFS::new());
        vfs_write(
            &root,
            "home/user/.openclaw/.env",
            "# comment\n\nANTHROPIC_API_KEY=\"sk-quoted\"\nOPENAI_API_KEY=\n",
        );

        let home = root.join("home/user").unwrap();
        let keys = detect_openclaw_api_keys_vfs(&home);
        assert_eq!(keys.anthropic.as_deref(), Some("sk-quoted"));
        // Empty value should be skipped
        assert!(keys.openai.is_none() || keys.openai.as_deref() != Some(""));
    }

    #[test]
    fn test_detected_keys_for_provider() {
        let keys = DetectedKeys {
            anthropic: Some("ant-key".to_string()),
            openai: Some("oai-key".to_string()),
        };
        assert_eq!(keys.for_provider("anthropic"), Some("ant-key"));
        assert_eq!(keys.for_provider("openai"), Some("oai-key"));
        assert_eq!(keys.for_provider("other"), None);

        let empty = DetectedKeys::default();
        assert_eq!(empty.for_provider("anthropic"), None);
    }
}
