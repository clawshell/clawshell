use serde_json::Value;
use std::error::Error;

use crate::onboard;

#[cfg(not(test))]
const OPENCLAW_GATEWAY_RELOAD_WORKAROUND_ISSUE_URL: &str =
    "https://github.com/openclaw/openclaw/issues/14161";

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenclawCommandOutput {
    pub success: bool,
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub trait OpenclawRunner {
    fn run(&mut self, args: &[String]) -> Result<OpenclawCommandOutput, String>;
}

#[derive(Debug, Default)]
pub struct RealOpenclawRunner;

impl OpenclawRunner for RealOpenclawRunner {
    fn run(&mut self, args: &[String]) -> Result<OpenclawCommandOutput, String> {
        let mut command = std::process::Command::new("openclaw");
        command.args(args.iter().map(String::as_str));
        command.env_remove("OPENAI_API_KEY");
        command.env_remove("ANTHROPIC_API_KEY");
        command.env_remove("ANTHROPIC_OAUTH_TOKEN");
        #[cfg(unix)]
        {
            if nix::unistd::geteuid().is_root() {
                let (uid, gid) = resolve_non_root_ids_for_openclaw()?;
                command.uid(uid);
                command.gid(gid);
                let user_env = resolve_non_root_user_env_for_openclaw(uid)?;
                command.env("HOME", user_env.home_dir);
                command.env("USER", user_env.username.as_str());
                command.env("LOGNAME", user_env.username.as_str());
            }
        }
        let output = command
            .output()
            .map_err(|error| format!("failed to spawn command: {error}"))?;
        Ok(OpenclawCommandOutput {
            success: output.status.success(),
            status_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

#[cfg(unix)]
fn resolve_non_root_ids_for_openclaw() -> Result<(u32, u32), String> {
    if let (Some(uid), Some(gid)) = (parse_env_u32("SUDO_UID"), parse_env_u32("SUDO_GID"))
        && uid > 0
        && gid > 0
    {
        return Ok((uid, gid));
    }

    if let Ok(user_name) = std::env::var("SUDO_USER")
        && !user_name.trim().is_empty()
        && user_name != "root"
    {
        match nix::unistd::User::from_name(&user_name) {
            Ok(Some(user)) => {
                let uid = user.uid.as_raw();
                let gid = user.gid.as_raw();
                if uid > 0 && gid > 0 {
                    return Ok((uid, gid));
                }
            }
            Ok(None) => {}
            Err(error) => {
                return Err(format!(
                    "failed to resolve SUDO_USER '{user_name}' for non-root openclaw execution: {error}"
                ));
            }
        }
    }

    Err(
        "refusing to run `openclaw` as root; please run clawshell with sudo from a regular user account."
            .to_string(),
    )
}

#[cfg(unix)]
fn parse_env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.parse::<u32>().ok()
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenclawTargetUserEnv {
    username: String,
    home_dir: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnixUserRecord {
    username: String,
    uid: u32,
    home_dir: String,
}

#[cfg(unix)]
fn resolve_non_root_user_env_for_openclaw(uid: u32) -> Result<OpenclawTargetUserEnv, String> {
    resolve_non_root_user_env_for_openclaw_with_lookup(
        uid,
        std::env::var("SUDO_USER").ok(),
        lookup_unix_user_by_name,
        lookup_unix_user_by_uid,
    )
}

#[cfg(unix)]
fn resolve_non_root_user_env_for_openclaw_with_lookup<FN, FU>(
    uid: u32,
    sudo_user: Option<String>,
    lookup_by_name: FN,
    lookup_by_uid: FU,
) -> Result<OpenclawTargetUserEnv, String>
where
    FN: Fn(&str) -> Result<Option<UnixUserRecord>, String>,
    FU: Fn(u32) -> Result<Option<UnixUserRecord>, String>,
{
    let normalized_sudo_user = sudo_user
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "root");

    if let Some(user_name) = normalized_sudo_user {
        let from_sudo_user = lookup_by_name(user_name)?;
        if let Some(record) = from_sudo_user
            && record.uid == uid
        {
            return map_record_to_target_env(record, uid);
        }
    }

    let from_uid = lookup_by_uid(uid)?;
    if let Some(record) = from_uid {
        return map_record_to_target_env(record, uid);
    }

    Err(format!(
        "failed to resolve non-root target account metadata for uid {uid}; could not determine HOME/USER/LOGNAME for `openclaw`."
    ))
}

#[cfg(unix)]
fn map_record_to_target_env(
    record: UnixUserRecord,
    target_uid: u32,
) -> Result<OpenclawTargetUserEnv, String> {
    let username = record.username.trim();
    if username.is_empty() {
        return Err(format!(
            "failed to resolve non-root target account metadata for uid {target_uid}; username is empty."
        ));
    }

    let home_dir = record.home_dir.trim();
    if home_dir.is_empty() {
        return Err(format!(
            "failed to resolve non-root target account metadata for uid {target_uid}; home directory is empty."
        ));
    }

    Ok(OpenclawTargetUserEnv {
        username: username.to_string(),
        home_dir: home_dir.to_string(),
    })
}

#[cfg(unix)]
fn lookup_unix_user_by_name(name: &str) -> Result<Option<UnixUserRecord>, String> {
    match nix::unistd::User::from_name(name) {
        Ok(Some(user)) => Ok(Some(UnixUserRecord {
            username: user.name,
            uid: user.uid.as_raw(),
            home_dir: user.dir.to_string_lossy().to_string(),
        })),
        Ok(None) => Ok(None),
        Err(error) => Err(format!(
            "failed to resolve user '{name}' for non-root openclaw execution: {error}"
        )),
    }
}

#[cfg(unix)]
fn lookup_unix_user_by_uid(uid: u32) -> Result<Option<UnixUserRecord>, String> {
    match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
        Ok(Some(user)) => Ok(Some(UnixUserRecord {
            username: user.name,
            uid: user.uid.as_raw(),
            home_dir: user.dir.to_string_lossy().to_string(),
        })),
        Ok(None) => Ok(None),
        Err(error) => Err(format!(
            "failed to resolve uid {uid} for non-root openclaw execution: {error}"
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallCleanupOutcome {
    BlockedByDefaultModel,
    Cleaned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenclawApprovalMode {
    PromptUser,
    AutoApprove,
}

pub fn run_openclaw_command<R: OpenclawRunner>(
    runner: &mut R,
    args: &[&str],
) -> Result<OpenclawCommandOutput, Box<dyn Error>> {
    run_openclaw_raw(
        runner,
        args.iter().map(|s| (*s).to_string()).collect(),
        OpenclawApprovalMode::PromptUser,
    )
}

pub fn apply_onboard_openclaw_config<R: OpenclawRunner>(
    runner: &mut R,
    config: &onboard::OnboardConfig,
) -> Result<(), Box<dyn Error>> {
    with_gateway_reload_mode_disabled(
        runner,
        "apply onboarding OpenClaw configuration changes",
        OpenclawApprovalMode::PromptUser,
        |runner| {
            let current_json =
                build_partial_config_for_mutation(runner, OpenclawApprovalMode::PromptUser)?;
            let current_content = serde_json::to_string(&current_json)?;
            let modified_content =
                onboard::patch_openclaw_config_for_clawshell(&current_content, config)?;
            let modified_json: Value = serde_json::from_str(&modified_content)?;

            openclaw_config_set_json(
                runner,
                "env",
                &nested_value_or_empty_object(&modified_json, &["env"]),
                OpenclawApprovalMode::PromptUser,
            )?;
            openclaw_config_set_json(
                runner,
                "agents.defaults.models",
                &nested_value_or_empty_object(&modified_json, &["agents", "defaults", "models"]),
                OpenclawApprovalMode::PromptUser,
            )?;
            openclaw_config_set_json(
                runner,
                "models.providers",
                &nested_value_or_empty_object(&modified_json, &["models", "providers"]),
                OpenclawApprovalMode::PromptUser,
            )?;
            Ok(())
        },
    )
}

pub fn cleanup_openclaw_for_uninstall<R: OpenclawRunner>(
    runner: &mut R,
    approval_mode: OpenclawApprovalMode,
) -> Result<UninstallCleanupOutcome, Box<dyn Error>> {
    let current_default_model = openclaw_config_get_string_optional_at_path(
        runner,
        "agents.defaults.model",
        approval_mode,
    )?;
    if current_default_model
        .as_deref()
        .is_some_and(is_clawshell_default_model_name)
    {
        return Ok(UninstallCleanupOutcome::BlockedByDefaultModel);
    }

    with_gateway_reload_mode_disabled(
        runner,
        "clean up OpenClaw configuration during uninstall",
        approval_mode,
        |runner| {
            let current_json = build_partial_config_for_cleanup(runner, approval_mode)?;
            let current_content = serde_json::to_string(&current_json)?;
            let cleaned_content = onboard::remove_clawshell_openclaw_entries(&current_content)?;
            let cleaned_json: Value = serde_json::from_str(&cleaned_content)?;

            openclaw_config_unset_path_if_exists(runner, "env.CLAWSHELL_API_KEY", approval_mode)?;
            openclaw_config_set_json(
                runner,
                "agents.defaults.models",
                &nested_value_or_empty_object(&cleaned_json, &["agents", "defaults", "models"]),
                approval_mode,
            )?;
            openclaw_config_unset_path_if_exists(
                runner,
                "models.providers.clawshell",
                approval_mode,
            )?;
            Ok(())
        },
    )?;
    Ok(UninstallCleanupOutcome::Cleaned)
}

fn set_gateway_reload_mode<R: OpenclawRunner>(
    runner: &mut R,
    mode: &str,
    approval_mode: OpenclawApprovalMode,
) -> Result<(), Box<dyn Error>> {
    let args = vec![
        "config".to_string(),
        "set".to_string(),
        "gateway.reload.mode".to_string(),
        mode.to_string(),
    ];
    run_openclaw_checked(runner, args, approval_mode)?;
    Ok(())
}

fn with_gateway_reload_mode_disabled<R, T, F>(
    runner: &mut R,
    operation_label: &str,
    approval_mode: OpenclawApprovalMode,
    operation: F,
) -> Result<T, Box<dyn Error>>
where
    R: OpenclawRunner,
    F: FnOnce(&mut R) -> Result<T, Box<dyn Error>>,
{
    set_gateway_reload_mode(runner, "off", approval_mode)
        .map_err(|error| format!("Failed to disable OpenClaw gateway reload mode: {error}"))?;

    let operation_result = operation(runner);
    let restore_result = set_gateway_reload_mode(runner, "hybrid", approval_mode)
        .map_err(|error| format!("Failed to restore OpenClaw gateway reload mode: {error}"));

    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(restore_error)) => Err(format!(
            "OpenClaw operation succeeded ({operation_label}), but {restore_error}"
        )
        .into()),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Err(operation_error), Err(restore_error)) => Err(format!(
            "OpenClaw operation failed ({operation_label}): {operation_error}; additionally, {restore_error}"
        )
        .into()),
    }
}

fn build_partial_config_for_mutation<R: OpenclawRunner>(
    runner: &mut R,
    approval_mode: OpenclawApprovalMode,
) -> Result<Value, Box<dyn Error>> {
    let env = openclaw_config_get_object_at_path_or_empty(runner, "env", approval_mode)?;
    let default_models = openclaw_config_get_object_at_path_or_empty(
        runner,
        "agents.defaults.models",
        approval_mode,
    )?;
    let providers =
        openclaw_config_get_object_at_path_or_empty(runner, "models.providers", approval_mode)?;
    Ok(serde_json::json!({
        "env": env,
        "agents": {
            "defaults": {
                "models": default_models,
            }
        },
        "models": {
            "providers": providers,
        }
    }))
}

fn build_partial_config_for_cleanup<R: OpenclawRunner>(
    runner: &mut R,
    approval_mode: OpenclawApprovalMode,
) -> Result<Value, Box<dyn Error>> {
    let env = openclaw_config_get_object_at_path_or_empty(runner, "env", approval_mode)?;
    let default_models = openclaw_config_get_object_at_path_or_empty(
        runner,
        "agents.defaults.models",
        approval_mode,
    )?;
    Ok(serde_json::json!({
        "env": env,
        "agents": {
            "defaults": {
                "models": default_models,
            }
        }
    }))
}

fn is_clawshell_default_model_name(model: &str) -> bool {
    model == "clawshell" || model.starts_with("clawshell/")
}

fn openclaw_config_get_object_at_path_or_empty<R: OpenclawRunner>(
    runner: &mut R,
    path: &str,
    approval_mode: OpenclawApprovalMode,
) -> Result<Value, Box<dyn Error>> {
    match openclaw_config_get_json_at_path(runner, path, approval_mode) {
        Ok(value) => match value {
            Value::Object(_) => Ok(value),
            Value::Null => Ok(serde_json::json!({})),
            _ => Err(format!(
                "`openclaw config get {path}` returned a non-object value; expected a JSON object."
            )
            .into()),
        },
        Err(error) if is_missing_config_path_error(&error.to_string()) => Ok(serde_json::json!({})),
        Err(error) => Err(error),
    }
}

fn openclaw_config_get_string_optional_at_path<R: OpenclawRunner>(
    runner: &mut R,
    path: &str,
    approval_mode: OpenclawApprovalMode,
) -> Result<Option<String>, Box<dyn Error>> {
    match openclaw_config_get_json_at_path(runner, path, approval_mode) {
        Ok(value) => extract_string_like_value(path, &value),
        Err(error) if is_missing_config_path_error(&error.to_string()) => Ok(None),
        Err(error) => Err(error),
    }
}

fn extract_string_like_value(path: &str, value: &Value) -> Result<Option<String>, Box<dyn Error>> {
    match value {
        Value::Null => Ok(None),
        Value::String(v) => Ok(Some(v.clone())),
        Value::Number(v) => Ok(Some(v.to_string())),
        Value::Bool(v) => Ok(Some(v.to_string())),
        Value::Object(map) => {
            for key in ["primary", "default", "model", "id", "name"] {
                if let Some(v) = map.get(key)
                    && let Some(as_string) = scalar_to_string(v)
                {
                    return Ok(Some(as_string));
                }
            }
            if map.len() == 1
                && let Some((_, only_value)) = map.iter().next()
                && let Some(as_string) = scalar_to_string(only_value)
            {
                return Ok(Some(as_string));
            }
            Err(format!(
                "`openclaw config get {path}` returned an unsupported object shape; expected a string-like model value or an object containing one of: primary/default/model/id/name."
            )
            .into())
        }
        Value::Array(_) => Err(format!(
            "`openclaw config get {path}` returned an array; expected a string-like model value."
        )
        .into()),
    }
}

fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(v) => Some(v.clone()),
        Value::Number(v) => Some(v.to_string()),
        Value::Bool(v) => Some(v.to_string()),
        _ => None,
    }
}

fn openclaw_config_get_json_at_path<R: OpenclawRunner>(
    runner: &mut R,
    path: &str,
    approval_mode: OpenclawApprovalMode,
) -> Result<Value, Box<dyn Error>> {
    let args = vec![
        "config".to_string(),
        "get".to_string(),
        path.to_string(),
        "--json".to_string(),
    ];
    let stdout = run_openclaw_checked(runner, args, approval_mode)?;
    parse_config_get_output(&stdout)
}

fn parse_config_get_output(output: &str) -> Result<Value, Box<dyn Error>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => Ok(value),
        Err(_) => Ok(Value::String(trimmed.to_string())),
    }
}

fn openclaw_config_set_json<R: OpenclawRunner>(
    runner: &mut R,
    path: &str,
    value: &Value,
    approval_mode: OpenclawApprovalMode,
) -> Result<(), Box<dyn Error>> {
    let payload = serde_json::to_string(value)?;
    let args = vec![
        "config".to_string(),
        "set".to_string(),
        path.to_string(),
        payload,
        "--json".to_string(),
    ];
    run_openclaw_checked(runner, args, approval_mode)?;
    Ok(())
}

fn openclaw_config_unset_path_if_exists<R: OpenclawRunner>(
    runner: &mut R,
    path: &str,
    approval_mode: OpenclawApprovalMode,
) -> Result<(), Box<dyn Error>> {
    let args = vec!["config".to_string(), "unset".to_string(), path.to_string()];
    let output = run_openclaw_raw(runner, args.clone(), approval_mode)?;
    if output.success {
        return Ok(());
    }

    let error = openclaw_command_failed(&args, &output);
    if is_missing_config_path_error(&error.to_string()) {
        return Ok(());
    }
    Err(error)
}

fn run_openclaw_checked<R: OpenclawRunner>(
    runner: &mut R,
    args: Vec<String>,
    approval_mode: OpenclawApprovalMode,
) -> Result<String, Box<dyn Error>> {
    let output = run_openclaw_raw(runner, args.clone(), approval_mode)?;
    if !output.success {
        return Err(openclaw_command_failed(&args, &output));
    }
    Ok(output.stdout)
}

fn run_openclaw_raw<R: OpenclawRunner>(
    runner: &mut R,
    args: Vec<String>,
    approval_mode: OpenclawApprovalMode,
) -> Result<OpenclawCommandOutput, Box<dyn Error>> {
    let display_args = args.join(" ");
    #[cfg(test)]
    let _ = approval_mode;
    #[cfg(not(test))]
    {
        if matches!(approval_mode, OpenclawApprovalMode::PromptUser) {
            let approval_message = if is_gateway_reload_mode_toggle_command(&args) {
                format!(
                    "Approve running `openclaw {display_args}` to work around issue {OPENCLAW_GATEWAY_RELOAD_WORKAROUND_ISSUE_URL}?"
                )
            } else {
                format!("Approve running `openclaw {display_args}`?")
            };
            let approved =
                crate::tui::prompt_confirm_compact(&approval_message, true).map_err(|error| {
                    format!("Failed to ask approval for `openclaw {display_args}`: {error}")
                })?;
            if !approved {
                return Err(format!("Command not approved: `openclaw {display_args}`").into());
            }
        }
    }
    runner.run(&args).map_err(|error| -> Box<dyn Error> {
        format!("Failed to run `openclaw {display_args}`: {error}").into()
    })
}

#[cfg(not(test))]
fn is_gateway_reload_mode_toggle_command(args: &[String]) -> bool {
    args.len() == 4
        && args[0] == "config"
        && args[1] == "set"
        && args[2] == "gateway.reload.mode"
        && (args[3] == "off" || args[3] == "hybrid")
}

fn openclaw_command_failed(args: &[String], output: &OpenclawCommandOutput) -> Box<dyn Error> {
    let stderr = output.stderr.trim();
    let stdout = output.stdout.trim();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "no command output"
    };
    format!(
        "`openclaw {}` failed (exit code {}): {}",
        args.join(" "),
        output
            .status_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        detail
    )
    .into()
}

fn is_missing_config_path_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if lower.contains("missing required argument 'path'") {
        return false;
    }
    lower.contains("path not found")
        || lower.contains("not found")
        || lower.contains("does not exist")
        || lower.contains("missing key")
        || lower.contains("missing path")
}

fn nested_value_or_empty_object(json: &Value, path: &[&str]) -> Value {
    let mut current = json;
    for key in path {
        let Some(next) = current.get(*key) else {
            return serde_json::json!({});
        };
        current = next;
    }
    current.clone()
}

use crate::onboard::{STATS_CRON_JOB_NAME, STATS_CRON_PROMPT};

const CHANNEL_PRIORITY: &[&str] = &["telegram", "discord", "slack", "mattermost"];

pub fn detect_openclaw_channel<R: OpenclawRunner>(runner: &mut R) -> Option<String> {
    let channels =
        openclaw_config_get_json_at_path(runner, "channels", OpenclawApprovalMode::AutoApprove)
            .ok()?;
    let obj = channels.as_object()?;
    for &platform in CHANNEL_PRIORITY {
        if let Some(cfg) = obj.get(platform) {
            if cfg.get("enabled").and_then(Value::as_bool) != Some(false) {
                return Some(platform.to_string());
            }
        }
    }
    None
}

pub fn setup_openclaw_stats_cron<R: OpenclawRunner>(
    runner: &mut R,
    channel: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let mut args: Vec<&str> = vec![
        "cron",
        "add",
        "--name",
        STATS_CRON_JOB_NAME,
        "--cron",
        "0 9 * * 1",
        "--session",
        "isolated",
        "--message",
        STATS_CRON_PROMPT,
    ];
    if let Some(ch) = channel {
        args.extend_from_slice(&["--announce", "--channel", ch]);
    } else {
        args.push("--no-deliver");
    }
    run_openclaw_command(runner, &args)?;
    Ok(())
}

pub fn remove_openclaw_stats_cron<R: OpenclawRunner>(runner: &mut R) -> Result<(), Box<dyn Error>> {
    run_openclaw_command(runner, &["cron", "remove", STATS_CRON_JOB_NAME])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    const PROMPT_MODE: OpenclawApprovalMode = OpenclawApprovalMode::PromptUser;
    const AUTO_MODE: OpenclawApprovalMode = OpenclawApprovalMode::AutoApprove;

    fn ok_output(stdout: &str) -> Result<OpenclawCommandOutput, String> {
        Ok(OpenclawCommandOutput {
            success: true,
            status_code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    fn failed_output(code: i32, stderr: &str) -> Result<OpenclawCommandOutput, String> {
        Ok(OpenclawCommandOutput {
            success: false,
            status_code: Some(code),
            stdout: String::new(),
            stderr: stderr.to_string(),
        })
    }

    #[derive(Debug, Default)]
    struct FakeOpenclawRunner {
        responses: VecDeque<Result<OpenclawCommandOutput, String>>,
        calls: Vec<Vec<String>>,
    }

    impl OpenclawRunner for FakeOpenclawRunner {
        fn run(&mut self, args: &[String]) -> Result<OpenclawCommandOutput, String> {
            self.calls.push(args.to_vec());
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err("no fake response queued".to_string()))
        }
    }

    fn test_onboard_config() -> onboard::OnboardConfig {
        onboard::OnboardConfig {
            provider: "openai".to_string(),
            model: "gpt-5".to_string(),
            auth_method: onboard::OnboardAuthMethod::StaticKey,
            real_api_key: "real_key".to_string(),
            virtual_api_key: "virtual_key".to_string(),
            target: onboard::OnboardTarget::Openclaw {
                config_path: PathBuf::from("/home/user/.openclaw/openclaw.json"),
            },
            server_host: "127.0.0.1".to_string(),
            server_port: 18790,
            email: None,
        }
    }

    fn queue_cleanup_success_responses(runner: &mut FakeOpenclawRunner) {
        runner.responses.push_back(ok_output("gpt-5"));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(
            r#"{"CLAWSHELL_API_KEY":"virtual_key","OTHER":"value"}"#,
        ));
        runner.responses.push_back(ok_output(
            r#"{"clawshell/gpt-5":{"alias":"clawshell"},"existing/model":{"alias":"existing"}}"#,
        ));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));
    }

    #[test]
    fn test_openclaw_config_get_json_at_path_parses_json() {
        let mut runner = FakeOpenclawRunner::default();
        runner
            .responses
            .push_back(ok_output(r#"{"CLAWSHELL_API_KEY":"abc"}"#));

        let json = openclaw_config_get_json_at_path(&mut runner, "env", PROMPT_MODE).unwrap();
        assert_eq!(json["CLAWSHELL_API_KEY"], "abc");
        assert_eq!(
            runner.calls,
            vec![vec![
                "config".to_string(),
                "get".to_string(),
                "env".to_string(),
                "--json".to_string()
            ]]
        );
    }

    #[test]
    fn test_openclaw_config_get_json_at_path_falls_back_to_string() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output("clawshell/gpt-5\n"));

        let value =
            openclaw_config_get_json_at_path(&mut runner, "agents.defaults.model", PROMPT_MODE)
                .unwrap();
        assert_eq!(value, Value::String("clawshell/gpt-5".to_string()));
    }

    #[test]
    fn test_openclaw_config_get_json_at_path_reports_command_failure() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(failed_output(2, "boom"));

        let error = openclaw_config_get_json_at_path(&mut runner, "env", PROMPT_MODE)
            .unwrap_err()
            .to_string();
        assert!(error.contains("openclaw config get env"));
        assert!(error.contains("boom"));
    }

    #[test]
    fn test_openclaw_config_get_object_at_path_or_empty_handles_missing_path() {
        let mut runner = FakeOpenclawRunner::default();
        runner
            .responses
            .push_back(failed_output(1, "path not found: env"));

        let value =
            openclaw_config_get_object_at_path_or_empty(&mut runner, "env", PROMPT_MODE).unwrap();
        assert_eq!(value, serde_json::json!({}));
    }

    #[test]
    fn test_openclaw_config_set_json_uses_expected_args() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output(""));

        openclaw_config_set_json(
            &mut runner,
            "env",
            &serde_json::json!({"CLAWSHELL_API_KEY":"abc"}),
            PROMPT_MODE,
        )
        .unwrap();

        assert_eq!(runner.calls.len(), 1);
        assert_eq!(runner.calls[0][0], "config");
        assert_eq!(runner.calls[0][1], "set");
        assert_eq!(runner.calls[0][2], "env");
        assert_eq!(runner.calls[0][4], "--json");
        let payload: Value = serde_json::from_str(&runner.calls[0][3]).unwrap();
        assert_eq!(payload["CLAWSHELL_API_KEY"], "abc");
    }

    #[test]
    fn test_openclaw_config_unset_path_if_exists_uses_expected_args() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output(""));

        openclaw_config_unset_path_if_exists(&mut runner, "env.CLAWSHELL_API_KEY", PROMPT_MODE)
            .unwrap();

        assert_eq!(
            runner.calls,
            vec![vec![
                "config".to_string(),
                "unset".to_string(),
                "env.CLAWSHELL_API_KEY".to_string()
            ]]
        );
    }

    #[test]
    fn test_openclaw_config_unset_path_if_exists_ignores_missing_path_error() {
        let mut runner = FakeOpenclawRunner::default();
        runner
            .responses
            .push_back(failed_output(1, "path not found: env.CLAWSHELL_API_KEY"));

        openclaw_config_unset_path_if_exists(&mut runner, "env.CLAWSHELL_API_KEY", PROMPT_MODE)
            .unwrap();
    }

    #[test]
    fn test_apply_onboard_openclaw_config_writes_expected_sections() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output(""));
        runner
            .responses
            .push_back(ok_output(r#"{"EXISTING":"true"}"#));
        runner
            .responses
            .push_back(ok_output(r#"{"existing/model":{"alias":"existing"}}"#));
        runner.responses.push_back(ok_output(
            r#"{"existing":{"baseUrl":"http://example.com"}}"#,
        ));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));

        apply_onboard_openclaw_config(&mut runner, &test_onboard_config()).unwrap();

        assert_eq!(runner.calls.len(), 8);
        assert_eq!(
            runner.calls[0],
            vec!["config", "set", "gateway.reload.mode", "off"]
        );
        assert_eq!(runner.calls[1], vec!["config", "get", "env", "--json"]);
        assert_eq!(
            runner.calls[2],
            vec!["config", "get", "agents.defaults.models", "--json"]
        );
        assert_eq!(
            runner.calls[3],
            vec!["config", "get", "models.providers", "--json"]
        );
        assert_eq!(runner.calls[4][2], "env");
        assert_eq!(runner.calls[5][2], "agents.defaults.models");
        assert_eq!(runner.calls[6][2], "models.providers");
        assert_eq!(
            runner.calls[7],
            vec!["config", "set", "gateway.reload.mode", "hybrid"]
        );

        let env_payload: Value = serde_json::from_str(&runner.calls[4][3]).unwrap();
        assert_eq!(env_payload["EXISTING"], "true");
        assert_eq!(env_payload["CLAWSHELL_API_KEY"], "virtual_key");

        let models_payload: Value = serde_json::from_str(&runner.calls[5][3]).unwrap();
        assert_eq!(models_payload["existing/model"]["alias"], "existing");
        assert_eq!(models_payload["clawshell/gpt-5"]["alias"], "clawshell");

        let providers_payload: Value = serde_json::from_str(&runner.calls[6][3]).unwrap();
        assert_eq!(
            providers_payload["existing"]["baseUrl"],
            "http://example.com"
        );
        assert_eq!(
            providers_payload["clawshell"]["baseUrl"],
            "http://127.0.0.1:18790/v1"
        );
    }

    #[test]
    fn test_cleanup_openclaw_for_uninstall_blocks_on_default_model() {
        let mut runner = FakeOpenclawRunner::default();
        runner
            .responses
            .push_back(ok_output(r#"{"primary":"clawshell/gpt-5.2-chat-latest"}"#));

        let outcome = cleanup_openclaw_for_uninstall(&mut runner, PROMPT_MODE).unwrap();
        assert_eq!(outcome, UninstallCleanupOutcome::BlockedByDefaultModel);
        assert_eq!(
            runner.calls,
            vec![vec![
                "config".to_string(),
                "get".to_string(),
                "agents.defaults.model".to_string(),
                "--json".to_string()
            ]]
        );
    }

    #[test]
    fn test_openclaw_config_get_string_optional_at_path_accepts_primary_object_shape() {
        let mut runner = FakeOpenclawRunner::default();
        runner
            .responses
            .push_back(ok_output(r#"{"primary":"clawshell/gpt-5.2-chat-latest"}"#));

        let value = openclaw_config_get_string_optional_at_path(
            &mut runner,
            "agents.defaults.model",
            PROMPT_MODE,
        )
        .unwrap();
        assert_eq!(value.as_deref(), Some("clawshell/gpt-5.2-chat-latest"));
    }

    #[test]
    fn test_cleanup_openclaw_for_uninstall_removes_clawshell_entries() {
        let mut runner = FakeOpenclawRunner::default();
        queue_cleanup_success_responses(&mut runner);

        let outcome = cleanup_openclaw_for_uninstall(&mut runner, PROMPT_MODE).unwrap();
        assert_eq!(outcome, UninstallCleanupOutcome::Cleaned);
        assert_eq!(runner.calls.len(), 8);
        assert_eq!(
            runner.calls[0],
            vec!["config", "get", "agents.defaults.model", "--json"]
        );
        assert_eq!(
            runner.calls[1],
            vec!["config", "set", "gateway.reload.mode", "off"]
        );
        assert_eq!(runner.calls[2], vec!["config", "get", "env", "--json"]);
        assert_eq!(
            runner.calls[3],
            vec!["config", "get", "agents.defaults.models", "--json"]
        );
        assert_eq!(
            runner.calls[4],
            vec!["config", "unset", "env.CLAWSHELL_API_KEY"]
        );
        assert_eq!(runner.calls[5][0], "config");
        assert_eq!(runner.calls[5][1], "set");
        assert_eq!(runner.calls[5][2], "agents.defaults.models");
        assert_eq!(
            runner.calls[6],
            vec!["config", "unset", "models.providers.clawshell"]
        );
        assert_eq!(
            runner.calls[7],
            vec!["config", "set", "gateway.reload.mode", "hybrid"]
        );

        let models_payload: Value = serde_json::from_str(&runner.calls[5][3]).unwrap();
        assert_eq!(models_payload["existing/model"]["alias"], "existing");
        assert!(models_payload.get("clawshell/gpt-5").is_none());
        assert_eq!(runner.calls[5][4], "--json");
    }

    #[test]
    fn test_cleanup_openclaw_for_uninstall_autoapprove_matches_prompt_mode_sequence() {
        let mut prompt_runner = FakeOpenclawRunner::default();
        let mut auto_runner = FakeOpenclawRunner::default();
        queue_cleanup_success_responses(&mut prompt_runner);
        queue_cleanup_success_responses(&mut auto_runner);

        let prompt_outcome =
            cleanup_openclaw_for_uninstall(&mut prompt_runner, PROMPT_MODE).unwrap();
        let auto_outcome = cleanup_openclaw_for_uninstall(&mut auto_runner, AUTO_MODE).unwrap();

        assert_eq!(prompt_outcome, UninstallCleanupOutcome::Cleaned);
        assert_eq!(auto_outcome, UninstallCleanupOutcome::Cleaned);
        assert_eq!(prompt_runner.calls, auto_runner.calls);
    }

    #[test]
    fn test_with_gateway_reload_mode_disabled_restores_hybrid_when_operation_fails() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output(""));
        runner
            .responses
            .push_back(failed_output(2, "mutation failed"));
        runner.responses.push_back(ok_output(""));

        let result = with_gateway_reload_mode_disabled(
            &mut runner,
            "test operation",
            PROMPT_MODE,
            |runner| {
                openclaw_config_set_json(runner, "env", &serde_json::json!({"k":"v"}), PROMPT_MODE)
            },
        );

        let error = result.unwrap_err().to_string();
        assert!(error.contains("mutation failed"));
        assert_eq!(
            runner.calls,
            vec![
                vec![
                    "config".to_string(),
                    "set".to_string(),
                    "gateway.reload.mode".to_string(),
                    "off".to_string()
                ],
                vec![
                    "config".to_string(),
                    "set".to_string(),
                    "env".to_string(),
                    "{\"k\":\"v\"}".to_string(),
                    "--json".to_string()
                ],
                vec![
                    "config".to_string(),
                    "set".to_string(),
                    "gateway.reload.mode".to_string(),
                    "hybrid".to_string()
                ]
            ]
        );
    }

    #[test]
    fn test_with_gateway_reload_mode_disabled_reports_restore_failure_after_success() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));
        runner
            .responses
            .push_back(failed_output(2, "restore failed"));

        let result = with_gateway_reload_mode_disabled(
            &mut runner,
            "test operation",
            PROMPT_MODE,
            |runner| {
                openclaw_config_set_json(runner, "env", &serde_json::json!({"k":"v"}), PROMPT_MODE)
            },
        );

        let error = result.unwrap_err().to_string();
        assert!(error.contains("test operation"));
        assert!(error.contains("restore failed"));
    }

    #[test]
    fn test_with_gateway_reload_mode_disabled_reports_both_failures() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output(""));
        runner
            .responses
            .push_back(failed_output(2, "mutation failed"));
        runner
            .responses
            .push_back(failed_output(3, "restore failed"));

        let result = with_gateway_reload_mode_disabled(
            &mut runner,
            "test operation",
            PROMPT_MODE,
            |runner| {
                openclaw_config_set_json(runner, "env", &serde_json::json!({"k":"v"}), PROMPT_MODE)
            },
        );

        let error = result.unwrap_err().to_string();
        assert!(error.contains("test operation"));
        assert!(error.contains("mutation failed"));
        assert!(error.contains("restore failed"));
    }

    #[test]
    fn test_with_gateway_reload_mode_disabled_does_not_run_operation_when_disable_fails() {
        let mut runner = FakeOpenclawRunner::default();
        runner
            .responses
            .push_back(failed_output(2, "disable failed"));
        let operation_called = Cell::new(false);

        let result = with_gateway_reload_mode_disabled(
            &mut runner,
            "test operation",
            PROMPT_MODE,
            |_runner| {
                operation_called.set(true);
                Ok(())
            },
        );

        let error = result.unwrap_err().to_string();
        assert!(error.contains("disable failed"));
        assert!(!operation_called.get());
        assert_eq!(
            runner.calls,
            vec![vec![
                "config".to_string(),
                "set".to_string(),
                "gateway.reload.mode".to_string(),
                "off".to_string()
            ]]
        );
    }

    #[cfg(unix)]
    fn fake_user(name: &str, uid: u32, home: &str) -> UnixUserRecord {
        UnixUserRecord {
            username: name.to_string(),
            uid,
            home_dir: home.to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_non_root_user_env_prefers_matching_sudo_user() {
        let resolved = resolve_non_root_user_env_for_openclaw_with_lookup(
            1000,
            Some("dev".to_string()),
            |name| {
                if name == "dev" {
                    Ok(Some(fake_user("dev", 1000, "/home/dev")))
                } else {
                    Ok(None)
                }
            },
            |_uid| Ok(Some(fake_user("fallback", 1000, "/home/fallback"))),
        )
        .unwrap();

        assert_eq!(
            resolved,
            OpenclawTargetUserEnv {
                username: "dev".to_string(),
                home_dir: "/home/dev".to_string(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_non_root_user_env_falls_back_to_uid_lookup() {
        let resolved = resolve_non_root_user_env_for_openclaw_with_lookup(
            1000,
            Some("dev".to_string()),
            |_name| Ok(Some(fake_user("dev", 2000, "/home/dev"))),
            |uid| {
                assert_eq!(uid, 1000);
                Ok(Some(fake_user("actual", 1000, "/home/actual")))
            },
        )
        .unwrap();

        assert_eq!(
            resolved,
            OpenclawTargetUserEnv {
                username: "actual".to_string(),
                home_dir: "/home/actual".to_string(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_non_root_user_env_errors_when_metadata_missing() {
        let error = resolve_non_root_user_env_for_openclaw_with_lookup(
            1000,
            Some("dev".to_string()),
            |_name| Ok(None),
            |_uid| Ok(None),
        )
        .unwrap_err();

        assert!(error.contains("failed to resolve non-root target account metadata"));
        assert!(error.contains("uid 1000"));
    }

    #[test]
    fn test_setup_openclaw_stats_cron_no_channel() {
        let mut runner = FakeOpenclawRunner {
            responses: VecDeque::from([ok_output("")]),
            ..Default::default()
        };
        setup_openclaw_stats_cron(&mut runner, None).unwrap();
        let args = &runner.calls[0];
        assert!(args.contains(&"--no-deliver".to_string()));
        assert!(!args.contains(&"--announce".to_string()));
    }

    #[test]
    fn test_setup_openclaw_stats_cron_with_channel() {
        let mut runner = FakeOpenclawRunner {
            responses: VecDeque::from([ok_output("")]),
            ..Default::default()
        };
        setup_openclaw_stats_cron(&mut runner, Some("telegram")).unwrap();
        let args = &runner.calls[0];
        assert!(args.contains(&"--announce".to_string()));
        assert!(args.contains(&"--channel".to_string()));
        assert!(args.contains(&"telegram".to_string()));
        assert!(!args.contains(&"--no-deliver".to_string()));
    }

    #[test]
    fn test_detect_openclaw_channel_finds_telegram() {
        let channels = r#"{"telegram": {"botToken": "123:ABC"}, "discord": {"enabled": false}}"#;
        let mut runner = FakeOpenclawRunner {
            responses: VecDeque::from([ok_output(channels)]),
            ..Default::default()
        };
        assert_eq!(
            detect_openclaw_channel(&mut runner),
            Some("telegram".to_string())
        );
    }

    #[test]
    fn test_detect_openclaw_channel_skips_disabled() {
        let channels = r#"{"telegram": {"enabled": false}}"#;
        let mut runner = FakeOpenclawRunner {
            responses: VecDeque::from([ok_output(channels)]),
            ..Default::default()
        };
        assert_eq!(detect_openclaw_channel(&mut runner), None);
    }

    #[test]
    fn test_detect_openclaw_channel_returns_none_on_missing() {
        let mut runner = FakeOpenclawRunner {
            responses: VecDeque::from([failed_output(1, "missing config path")]),
            ..Default::default()
        };
        assert_eq!(detect_openclaw_channel(&mut runner), None);
    }

    #[test]
    fn test_remove_openclaw_stats_cron_sends_correct_args() {
        let mut runner = FakeOpenclawRunner {
            responses: VecDeque::from([ok_output("")]),
            ..Default::default()
        };
        remove_openclaw_stats_cron(&mut runner).unwrap();
        assert_eq!(runner.calls.len(), 1);
        assert_eq!(runner.calls[0], vec!["cron", "remove", STATS_CRON_JOB_NAME]);
    }
}
