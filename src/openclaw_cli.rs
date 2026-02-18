use serde_json::Value;
use std::error::Error;

use crate::onboard;

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
        #[cfg(unix)]
        {
            if nix::unistd::geteuid().is_root() {
                let (uid, gid) = resolve_non_root_ids_for_openclaw()?;
                command.uid(uid);
                command.gid(gid);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallCleanupOutcome {
    BlockedByDefaultModel,
    Cleaned,
}

pub fn run_openclaw_command<R: OpenclawRunner>(
    runner: &mut R,
    args: &[&str],
) -> Result<OpenclawCommandOutput, Box<dyn Error>> {
    run_openclaw_raw(runner, args.iter().map(|s| (*s).to_string()).collect())
}

pub fn apply_onboard_openclaw_config<R: OpenclawRunner>(
    runner: &mut R,
    config: &onboard::OnboardConfig,
) -> Result<(), Box<dyn Error>> {
    let current_json = build_partial_config_for_mutation(runner)?;
    let current_content = serde_json::to_string(&current_json)?;
    let modified_content = onboard::modify_openclaw_config(&current_content, config)?;
    let modified_json: Value = serde_json::from_str(&modified_content)?;

    openclaw_config_set_json(
        runner,
        "env",
        &nested_value_or_empty_object(&modified_json, &["env"]),
    )?;
    openclaw_config_set_json(
        runner,
        "agents.defaults.models",
        &nested_value_or_empty_object(&modified_json, &["agents", "defaults", "models"]),
    )?;
    openclaw_config_set_json(
        runner,
        "models.providers",
        &nested_value_or_empty_object(&modified_json, &["models", "providers"]),
    )?;
    Ok(())
}

pub fn cleanup_openclaw_for_uninstall<R: OpenclawRunner>(
    runner: &mut R,
) -> Result<UninstallCleanupOutcome, Box<dyn Error>> {
    let current_default_model =
        openclaw_config_get_string_optional_at_path(runner, "agents.defaults.model")?;
    if current_default_model
        .as_deref()
        .is_some_and(is_clawshell_default_model_name)
    {
        return Ok(UninstallCleanupOutcome::BlockedByDefaultModel);
    }

    let current_json = build_partial_config_for_mutation(runner)?;
    let current_content = serde_json::to_string(&current_json)?;
    let cleaned_content = onboard::remove_openclaw_entries(&current_content)?;
    let cleaned_json: Value = serde_json::from_str(&cleaned_content)?;

    openclaw_config_unset_path_if_exists(runner, "env.CLAWSHELL_API_KEY")?;
    openclaw_config_set_json(
        runner,
        "agents.defaults.models",
        &nested_value_or_empty_object(&cleaned_json, &["agents", "defaults", "models"]),
    )?;
    openclaw_config_set_json(
        runner,
        "models.providers",
        &nested_value_or_empty_object(&cleaned_json, &["models", "providers"]),
    )?;
    Ok(UninstallCleanupOutcome::Cleaned)
}

fn build_partial_config_for_mutation<R: OpenclawRunner>(
    runner: &mut R,
) -> Result<Value, Box<dyn Error>> {
    let env = openclaw_config_get_object_at_path_or_empty(runner, "env")?;
    let default_models =
        openclaw_config_get_object_at_path_or_empty(runner, "agents.defaults.models")?;
    let providers = openclaw_config_get_object_at_path_or_empty(runner, "models.providers")?;
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

fn is_clawshell_default_model_name(model: &str) -> bool {
    model == "clawshell" || model.starts_with("clawshell/")
}

fn openclaw_config_get_object_at_path_or_empty<R: OpenclawRunner>(
    runner: &mut R,
    path: &str,
) -> Result<Value, Box<dyn Error>> {
    match openclaw_config_get_json_at_path(runner, path) {
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
) -> Result<Option<String>, Box<dyn Error>> {
    match openclaw_config_get_json_at_path(runner, path) {
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
) -> Result<Value, Box<dyn Error>> {
    let args = vec![
        "config".to_string(),
        "get".to_string(),
        path.to_string(),
        "--json".to_string(),
    ];
    let stdout = run_openclaw_checked(runner, args)?;
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
) -> Result<(), Box<dyn Error>> {
    let payload = serde_json::to_string(value)?;
    let args = vec![
        "config".to_string(),
        "set".to_string(),
        path.to_string(),
        payload,
        "--json".to_string(),
    ];
    run_openclaw_checked(runner, args)?;
    Ok(())
}

fn openclaw_config_unset_path_if_exists<R: OpenclawRunner>(
    runner: &mut R,
    path: &str,
) -> Result<(), Box<dyn Error>> {
    let args = vec!["config".to_string(), "unset".to_string(), path.to_string()];
    let output = run_openclaw_raw(runner, args.clone())?;
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
) -> Result<String, Box<dyn Error>> {
    let output = run_openclaw_raw(runner, args.clone())?;
    if !output.success {
        return Err(openclaw_command_failed(&args, &output));
    }
    Ok(output.stdout)
}

fn run_openclaw_raw<R: OpenclawRunner>(
    runner: &mut R,
    args: Vec<String>,
) -> Result<OpenclawCommandOutput, Box<dyn Error>> {
    let display_args = args.join(" ");
    #[cfg(not(test))]
    {
        let approved = crate::tui::prompt_confirm(
            &format!("Approve running `openclaw {display_args}`?"),
            true,
        )
        .map_err(|error| {
            format!("Failed to ask approval for `openclaw {display_args}`: {error}")
        })?;
        if !approved {
            return Err(format!("Command not approved: `openclaw {display_args}`").into());
        }
    }
    runner.run(&args).map_err(|error| -> Box<dyn Error> {
        format!("Failed to run `openclaw {display_args}`: {error}").into()
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;

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
            real_api_key: "real_key".to_string(),
            virtual_api_key: "virtual_key".to_string(),
            openclaw_config_path: PathBuf::from("/home/user/.openclaw/openclaw.json"),
            server_host: "127.0.0.1".to_string(),
            server_port: 18790,
            email: None,
        }
    }

    #[test]
    fn test_openclaw_config_get_json_at_path_parses_json() {
        let mut runner = FakeOpenclawRunner::default();
        runner
            .responses
            .push_back(ok_output(r#"{"CLAWSHELL_API_KEY":"abc"}"#));

        let json = openclaw_config_get_json_at_path(&mut runner, "env").unwrap();
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

        let value = openclaw_config_get_json_at_path(&mut runner, "agents.defaults.model").unwrap();
        assert_eq!(value, Value::String("clawshell/gpt-5".to_string()));
    }

    #[test]
    fn test_openclaw_config_get_json_at_path_reports_command_failure() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(failed_output(2, "boom"));

        let error = openclaw_config_get_json_at_path(&mut runner, "env")
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

        let value = openclaw_config_get_object_at_path_or_empty(&mut runner, "env").unwrap();
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

        openclaw_config_unset_path_if_exists(&mut runner, "env.CLAWSHELL_API_KEY").unwrap();

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

        openclaw_config_unset_path_if_exists(&mut runner, "env.CLAWSHELL_API_KEY").unwrap();
    }

    #[test]
    fn test_apply_onboard_openclaw_config_writes_expected_sections() {
        let mut runner = FakeOpenclawRunner::default();
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

        apply_onboard_openclaw_config(&mut runner, &test_onboard_config()).unwrap();

        assert_eq!(runner.calls.len(), 6);
        assert_eq!(runner.calls[0], vec!["config", "get", "env", "--json"]);
        assert_eq!(
            runner.calls[1],
            vec!["config", "get", "agents.defaults.models", "--json"]
        );
        assert_eq!(
            runner.calls[2],
            vec!["config", "get", "models.providers", "--json"]
        );
        assert_eq!(runner.calls[3][2], "env");
        assert_eq!(runner.calls[4][2], "agents.defaults.models");
        assert_eq!(runner.calls[5][2], "models.providers");

        let env_payload: Value = serde_json::from_str(&runner.calls[3][3]).unwrap();
        assert_eq!(env_payload["EXISTING"], "true");
        assert_eq!(env_payload["CLAWSHELL_API_KEY"], "virtual_key");

        let models_payload: Value = serde_json::from_str(&runner.calls[4][3]).unwrap();
        assert_eq!(models_payload["existing/model"]["alias"], "existing");
        assert_eq!(models_payload["clawshell/gpt-5"]["alias"], "clawshell");

        let providers_payload: Value = serde_json::from_str(&runner.calls[5][3]).unwrap();
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

        let outcome = cleanup_openclaw_for_uninstall(&mut runner).unwrap();
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

        let value =
            openclaw_config_get_string_optional_at_path(&mut runner, "agents.defaults.model")
                .unwrap();
        assert_eq!(value.as_deref(), Some("clawshell/gpt-5.2-chat-latest"));
    }

    #[test]
    fn test_cleanup_openclaw_for_uninstall_removes_clawshell_entries() {
        let mut runner = FakeOpenclawRunner::default();
        runner.responses.push_back(ok_output("gpt-5"));
        runner.responses.push_back(ok_output(
            r#"{"CLAWSHELL_API_KEY":"virtual_key","OTHER":"value"}"#,
        ));
        runner.responses.push_back(ok_output(
            r#"{"clawshell/gpt-5":{"alias":"clawshell"},"existing/model":{"alias":"existing"}}"#,
        ));
        runner.responses.push_back(ok_output(
            r#"{"clawshell":{"baseUrl":"http://127.0.0.1:18790/v1"},"openai":{"baseUrl":"https://api.openai.com/v1"}}"#,
        ));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));
        runner.responses.push_back(ok_output(""));

        let outcome = cleanup_openclaw_for_uninstall(&mut runner).unwrap();
        assert_eq!(outcome, UninstallCleanupOutcome::Cleaned);
        assert_eq!(runner.calls.len(), 7);
        assert_eq!(
            runner.calls[0],
            vec!["config", "get", "agents.defaults.model", "--json"]
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
        assert_eq!(
            runner.calls[4],
            vec!["config", "unset", "env.CLAWSHELL_API_KEY"]
        );
        assert_eq!(runner.calls[5][2], "agents.defaults.models");
        assert_eq!(runner.calls[6][2], "models.providers");

        let models_payload: Value = serde_json::from_str(&runner.calls[5][3]).unwrap();
        assert_eq!(models_payload["existing/model"]["alias"], "existing");
        assert!(models_payload.get("clawshell/gpt-5").is_none());

        let providers_payload: Value = serde_json::from_str(&runner.calls[6][3]).unwrap();
        assert_eq!(
            providers_payload["openai"]["baseUrl"],
            "https://api.openai.com/v1"
        );
        assert!(providers_payload.get("clawshell").is_none());
    }
}
