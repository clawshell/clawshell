//! Minimal runner for shelling out to the Hermes Agent CLI during onboarding.
//!
//! Mirrors the shape of `openclaw_cli::OpenclawRunner`: a trait so tests can
//! inject a fake, and a `Real*` implementation that drops root privileges
//! when clawshell itself was invoked with `sudo` (Hermes lives under the
//! user's `~/.hermes/`, not root's).
//!
//! This runner is deliberately narrow — it only knows how to invoke
//! `hermes config set <key> <value>` sequences built by
//! `crate::onboard::hermes_config_set_commands`.

use crate::onboard;
use std::error::Error;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HermesCommandOutput {
    pub success: bool,
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub trait HermesRunner {
    fn run(&mut self, args: &[String]) -> Result<HermesCommandOutput, String>;
}

#[derive(Debug, Default)]
pub struct RealHermesRunner;

impl HermesRunner for RealHermesRunner {
    fn run(&mut self, args: &[String]) -> Result<HermesCommandOutput, String> {
        let mut command = std::process::Command::new("hermes");
        command.args(args.iter().map(String::as_str));
        #[cfg(unix)]
        {
            if nix::unistd::geteuid().is_root() {
                let (uid, gid) = resolve_non_root_ids()?;
                command.uid(uid);
                command.gid(gid);
                let (username, home_dir) = resolve_non_root_user_env(uid)?;
                command.env("HOME", home_dir);
                command.env("USER", &username);
                command.env("LOGNAME", &username);
            }
        }
        let output = command
            .output()
            .map_err(|error| format!("failed to spawn `hermes`: {error}"))?;
        Ok(HermesCommandOutput {
            success: output.status.success(),
            status_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

#[cfg(unix)]
fn resolve_non_root_ids() -> Result<(u32, u32), String> {
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
                    "failed to resolve SUDO_USER '{user_name}' for non-root hermes execution: {error}"
                ));
            }
        }
    }

    Err(
        "refusing to run `hermes` as root; please run clawshell with sudo from a regular user account."
            .to_string(),
    )
}

#[cfg(unix)]
fn parse_env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.parse::<u32>().ok()
}

#[cfg(unix)]
fn resolve_non_root_user_env(uid: u32) -> Result<(String, String), String> {
    if let Ok(user_name) = std::env::var("SUDO_USER")
        && !user_name.trim().is_empty()
        && user_name != "root"
        && let Ok(Some(user)) = nix::unistd::User::from_name(user_name.trim())
        && user.uid.as_raw() == uid
    {
        let home = user.dir.to_string_lossy().to_string();
        if !home.is_empty() {
            return Ok((user.name, home));
        }
    }

    match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
        Ok(Some(user)) => {
            let home = user.dir.to_string_lossy().to_string();
            if home.is_empty() {
                return Err(format!(
                    "failed to resolve home directory for uid {uid} when running `hermes`."
                ));
            }
            Ok((user.name, home))
        }
        Ok(None) => Err(format!(
            "failed to resolve account metadata for uid {uid} when running `hermes`."
        )),
        Err(error) => Err(format!(
            "failed to resolve uid {uid} for non-root hermes execution: {error}"
        )),
    }
}

/// Apply the onboarding configuration to Hermes by running the sequence of
/// `hermes config set` commands built by `onboard::hermes_config_set_commands`.
/// Fails fast on the first non-zero exit.
pub fn apply_onboard_hermes_config<R: HermesRunner>(
    runner: &mut R,
    config: &onboard::OnboardConfig,
) -> Result<(), Box<dyn Error>> {
    let commands = onboard::hermes_config_set_commands(config);
    for args in commands {
        let human = format!("hermes {}", args.join(" "));
        let output = runner
            .run(&args)
            .map_err(|e| format!("failed to run `{human}`: {e}"))?;
        if !output.success {
            let status = output
                .status_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let stderr = output.stderr.trim();
            return Err(format!("`{human}` exited with status {status}: {stderr}").into());
        }
    }
    Ok(())
}

use crate::onboard::{STATS_CRON_JOB_NAME, STATS_CRON_PROMPT};
use std::path::Path;

const HERMES_PLATFORM_TOKENS: &[(&str, &str)] = &[
    ("TELEGRAM_BOT_TOKEN", "telegram"),
    ("DISCORD_BOT_TOKEN", "discord"),
    ("SLACK_BOT_TOKEN", "slack"),
];

pub fn detect_hermes_channel(home_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(home_dir.join(".hermes").join(".env")).ok()?;
    for &(key, platform) in HERMES_PLATFORM_TOKENS {
        for line in content.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(platform.to_string());
                }
            }
        }
    }
    None
}

pub fn setup_hermes_stats_cron<R: HermesRunner>(
    runner: &mut R,
    channel: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let mut args: Vec<String> = vec![
        "cron".into(),
        "create".into(),
        "0 9 * * 1".into(),
        STATS_CRON_PROMPT.into(),
        "--skill".into(),
        "get-clawshell-stats".into(),
        "--name".into(),
        STATS_CRON_JOB_NAME.into(),
    ];
    if let Some(ch) = channel {
        args.extend_from_slice(&["--deliver".into(), ch.into()]);
    }
    let output = runner
        .run(&args)
        .map_err(|e| format!("failed to run `hermes cron create`: {e}"))?;
    if !output.success {
        let status = output
            .status_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Err(format!(
            "`hermes cron create` exited with status {status}: {}",
            output.stderr.trim()
        )
        .into());
    }
    Ok(())
}

#[allow(dead_code)]
pub fn remove_hermes_stats_cron<R: HermesRunner>(runner: &mut R) -> Result<(), Box<dyn Error>> {
    let output = runner
        .run(&["cron".into(), "remove".into(), STATS_CRON_JOB_NAME.into()])
        .map_err(|e| format!("failed to run `hermes cron remove`: {e}"))?;
    if !output.success {
        let status = output
            .status_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Err(format!(
            "`hermes cron remove` exited with status {status}: {}",
            output.stderr.trim()
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::{OnboardAuthMethod, OnboardConfig, OnboardTarget};
    use std::collections::VecDeque;

    fn test_config() -> OnboardConfig {
        OnboardConfig {
            provider: "openai".to_string(),
            model: "gpt-5.2".to_string(),
            auth_method: OnboardAuthMethod::StaticKey,
            real_api_key: "sk-real-key-123".to_string(),
            virtual_api_key: "{clawshell-virtual-key-openai}".to_string(),
            target: OnboardTarget::Hermes,
            server_host: "127.0.0.1".to_string(),
            server_port: 18790,
            email: None,
        }
    }

    #[derive(Default)]
    struct FakeHermesRunner {
        calls: Vec<Vec<String>>,
        responses: VecDeque<Result<HermesCommandOutput, String>>,
    }

    impl HermesRunner for FakeHermesRunner {
        fn run(&mut self, args: &[String]) -> Result<HermesCommandOutput, String> {
            self.calls.push(args.to_vec());
            self.responses.pop_front().unwrap_or_else(|| {
                Ok(HermesCommandOutput {
                    success: true,
                    status_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            })
        }
    }

    fn ok() -> HermesCommandOutput {
        HermesCommandOutput {
            success: true,
            status_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn applies_all_four_config_set_calls_in_order() {
        let mut runner = FakeHermesRunner::default();
        for _ in 0..4 {
            runner.responses.push_back(Ok(ok()));
        }

        apply_onboard_hermes_config(&mut runner, &test_config()).unwrap();

        assert_eq!(runner.calls.len(), 4);
        assert_eq!(runner.calls[0][..3], ["config", "set", "model.provider"]);
        assert_eq!(runner.calls[0][3], "custom");
        assert_eq!(runner.calls[1][2], "model.base_url");
        assert_eq!(runner.calls[1][3], "http://127.0.0.1:18790/v1");
        assert_eq!(runner.calls[2][2], "model.default");
        assert_eq!(runner.calls[3][2], "model.api_key");
        assert_eq!(runner.calls[3][3], "{clawshell-virtual-key-openai}");
    }

    #[test]
    fn stops_on_first_failure_and_reports_stderr() {
        let mut runner = FakeHermesRunner::default();
        runner.responses.push_back(Ok(ok()));
        runner.responses.push_back(Ok(HermesCommandOutput {
            success: false,
            status_code: Some(2),
            stdout: String::new(),
            stderr: "invalid key".to_string(),
        }));

        let err =
            apply_onboard_hermes_config(&mut runner, &test_config()).expect_err("should fail");
        let msg = err.to_string();
        assert!(msg.contains("status 2"), "msg: {msg}");
        assert!(msg.contains("invalid key"), "msg: {msg}");
        assert!(
            msg.contains("hermes config set model.base_url"),
            "msg: {msg}"
        );

        // First two calls ran; last two were never attempted.
        assert_eq!(runner.calls.len(), 2);
    }

    #[test]
    fn propagates_spawn_errors() {
        let mut runner = FakeHermesRunner::default();
        runner
            .responses
            .push_back(Err("no such binary: hermes".to_string()));

        let err =
            apply_onboard_hermes_config(&mut runner, &test_config()).expect_err("should fail");
        let msg = err.to_string();
        assert!(msg.contains("failed to run"), "msg: {msg}");
        assert!(msg.contains("no such binary"), "msg: {msg}");
    }

    #[test]
    fn test_setup_hermes_stats_cron_no_channel() {
        let mut runner = FakeHermesRunner::default();
        setup_hermes_stats_cron(&mut runner, None).unwrap();
        assert_eq!(runner.calls.len(), 1);
        let args = &runner.calls[0];
        assert_eq!(args[0], "cron");
        assert_eq!(args[1], "create");
        assert!(args[3].contains("get-clawshell-stats"));
        assert!(!args.contains(&"--deliver".to_string()));
    }

    #[test]
    fn test_setup_hermes_stats_cron_with_channel() {
        let mut runner = FakeHermesRunner::default();
        setup_hermes_stats_cron(&mut runner, Some("discord")).unwrap();
        assert_eq!(runner.calls.len(), 1);
        let args = &runner.calls[0];
        assert!(args.contains(&"--deliver".to_string()));
        assert!(args.contains(&"discord".to_string()));
    }

    #[test]
    fn test_detect_hermes_channel_finds_discord() {
        let dir = tempfile::tempdir().unwrap();
        let hermes_dir = dir.path().join(".hermes");
        std::fs::create_dir_all(&hermes_dir).unwrap();
        std::fs::write(
            hermes_dir.join(".env"),
            "DISCORD_BOT_TOKEN=abc123\nSOME_OTHER=val\n",
        )
        .unwrap();
        assert_eq!(
            detect_hermes_channel(dir.path()),
            Some("discord".to_string())
        );
    }

    #[test]
    fn test_detect_hermes_channel_ignores_empty_value() {
        let dir = tempfile::tempdir().unwrap();
        let hermes_dir = dir.path().join(".hermes");
        std::fs::create_dir_all(&hermes_dir).unwrap();
        std::fs::write(hermes_dir.join(".env"), "DISCORD_BOT_TOKEN=\n").unwrap();
        assert_eq!(detect_hermes_channel(dir.path()), None);
    }

    #[test]
    fn test_detect_hermes_channel_returns_none_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_hermes_channel(dir.path()), None);
    }

    #[test]
    fn test_remove_hermes_stats_cron_sends_correct_args() {
        let mut runner = FakeHermesRunner::default();
        remove_hermes_stats_cron(&mut runner).unwrap();
        assert_eq!(runner.calls.len(), 1);
        assert_eq!(runner.calls[0], vec!["cron", "remove", STATS_CRON_JOB_NAME]);
    }
}
