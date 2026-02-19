#![deny(warnings)]
#![deny(unsafe_code)] // why would we need unsafe code in this project?
#![deny(missing_debug_implementations)]

mod app;
mod cli;
mod config;
mod dlp;
mod email;
mod keys;
mod migration;
mod onboard;
mod openclaw_cli;
mod platform;
mod process;
mod proxy;
mod tui;

use clap::Parser;
use std::error::Error;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio::signal;
use tracing::{debug, info, warn};

use crate::app::{AppState, build_router};
use crate::cli::{Cli, Commands, OnAmbiguousOption};
use crate::config::Config;
use crate::migration::core::{
    AmbiguityResolutionError, AmbiguityResolver, AmbiguousChoice, MigrationIssue,
};
use crate::migration::orchestrator;
use crate::migration::target::MigrationTarget;
use crate::migration::targets::clawshell_toml::{self, ClawshellTomlTarget, VersionGateStatus};

#[derive(Debug)]
struct InteractiveAmbiguityResolver;

impl AmbiguityResolver for InteractiveAmbiguityResolver {
    fn resolve(
        &mut self,
        issue: &MigrationIssue,
    ) -> Result<AmbiguousChoice, AmbiguityResolutionError> {
        tui::print_warning(&format!(
            "Ambiguous migration step for target '{}' ({}): {}",
            issue.target, issue.step_id, issue.message
        ));
        tui::print_info("Recommended", &issue.recommended.to_string());

        let choice = tui::prompt_select(
            "How should migration proceed?",
            vec![
                "Apply recommended".to_string(),
                "Skip this step".to_string(),
                "Abort migration".to_string(),
            ],
        )
        .map_err(|e| AmbiguityResolutionError::Message(e.to_string()))?;

        let decision = match choice.as_str() {
            "Apply recommended" => AmbiguousChoice::ApplyRecommended,
            "Skip this step" => AmbiguousChoice::Skip,
            _ => AmbiguousChoice::Abort,
        };
        Ok(decision)
    }
}

#[derive(Debug)]
struct FailOnAmbiguousResolver;

impl AmbiguityResolver for FailOnAmbiguousResolver {
    fn resolve(
        &mut self,
        issue: &MigrationIssue,
    ) -> Result<AmbiguousChoice, AmbiguityResolutionError> {
        Err(AmbiguityResolutionError::Message(format!(
            "Ambiguous migration step '{}' for target '{}': {}. Re-run without --on-ambiguous fail to resolve interactively.",
            issue.step_id, issue.target, issue.message
        )))
    }
}

fn ensure_config_migrated(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    clawshell_toml::ensure_current_version(path).map_err(|e| {
        format!(
            "{}. Run 'clawshell migrate-config --config {}' to migrate to the current schema.",
            e,
            path.display()
        )
        .into()
    })
}

fn ensure_default_config_migrated_if_present() -> Result<(), Box<dyn Error>> {
    let path = process::default_config_path();
    if path.exists() {
        ensure_config_migrated(&path)?;
    }
    Ok(())
}

fn canonicalize_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    canonicalize_or_original(left) == canonicalize_or_original(right)
}

fn try_read_openclaw_config_path(clawshell_config_file: &Path) -> Option<PathBuf> {
    let config_content = std::fs::read_to_string(clawshell_config_file).ok()?;
    let config_json = serde_json::from_str::<serde_json::Value>(&config_content).ok()?;
    config_json
        .get("openclaw_config_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

#[derive(Debug, Clone)]
struct WrittenOpenclawSkill {
    path: PathBuf,
    manifest_entry: onboard::ManagedSkillManifestEntry,
}

fn write_onboard_openclaw_skill(
    ob_config: &crate::onboard::OnboardConfig,
) -> Result<Option<WrittenOpenclawSkill>, Box<dyn Error>> {
    let Some(skill) = onboard::render_openclaw_email_messages_skill(ob_config) else {
        return Ok(None);
    };

    let openclaw_root = onboard::openclaw_config_root(&ob_config.openclaw_config_path);
    let skill_dir = openclaw_root.join("skills").join(skill.name);
    std::fs::create_dir_all(&skill_dir)?;

    let mut managed_files: Vec<(String, String)> = Vec::new();
    for file in skill.files {
        let path = skill_dir.join(file.relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &file.content)?;
        managed_files.push((file.relative_path.to_string(), file.content));
    }

    let metadata = onboard::build_managed_skill_metadata(skill.name, &managed_files);
    onboard::write_managed_skill_metadata(&skill_dir, &metadata)?;
    let manifest_entry = onboard::build_managed_skill_manifest_entry(&skill_dir, &metadata);

    if !align_owner_with_openclaw_path(&skill_dir, &ob_config.openclaw_config_path)? {
        warn!(
            path = %skill_dir.display(),
            openclaw_path = %ob_config.openclaw_config_path.display(),
            "Could not determine OpenClaw file owner while writing skill files; will retry later"
        );
    }

    Ok(Some(WrittenOpenclawSkill {
        path: skill_dir,
        manifest_entry,
    }))
}

fn resolve_openclaw_owner_spec(openclaw_path: &Path) -> Result<Option<String>, Box<dyn Error>> {
    use std::os::unix::fs::MetadataExt;

    let read_owner = |path: &Path| -> Result<(u32, u32), Box<dyn Error>> {
        let metadata = std::fs::metadata(path)?;
        Ok((metadata.uid(), metadata.gid()))
    };

    let file_owner = if openclaw_path.exists() {
        Some(read_owner(openclaw_path)?)
    } else {
        None
    };
    let parent_owner = openclaw_path
        .parent()
        .filter(|parent| parent.exists())
        .map(read_owner)
        .transpose()?;
    let root_entry_owner = {
        let root = onboard::openclaw_config_root(openclaw_path);
        if root.exists() {
            let mut found = None;
            for entry in std::fs::read_dir(root)? {
                let entry = entry?;
                found = Some(read_owner(&entry.path())?);
                if let Some((uid, _)) = found
                    && uid != 0
                {
                    break;
                }
            }
            found
        } else {
            None
        }
    };

    let preferred = file_owner
        .filter(|(uid, _)| *uid != 0)
        .or_else(|| root_entry_owner.filter(|(uid, _)| *uid != 0))
        .or_else(|| parent_owner.filter(|(uid, _)| *uid != 0))
        .or(file_owner)
        .or(root_entry_owner)
        .or(parent_owner);

    Ok(preferred.map(|(uid, gid)| format!("{uid}:{gid}")))
}

fn chown_path(path: &Path, owner_spec: &str, recursive: bool) -> Result<(), Box<dyn Error>> {
    let mut command = std::process::Command::new("chown");
    if recursive {
        command.arg("-R");
    }
    let path_arg = path.to_string_lossy().into_owned();
    command.args([owner_spec, path_arg.as_str()]);
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(format!(
        "chown failed for '{}' with owner '{}': status={}, stderr={}",
        path.display(),
        owner_spec,
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
    .into())
}

fn align_owner_with_openclaw_path(
    path: &Path,
    openclaw_path: &Path,
) -> Result<bool, Box<dyn Error>> {
    let Some(owner_spec) = resolve_openclaw_owner_spec(openclaw_path)? else {
        return Ok(false);
    };
    chown_path(path, &owner_spec, true)?;
    Ok(true)
}

fn print_openclaw_recovery_notice(openclaw_path: &Path, backup_path: &Path) {
    let bak = backup_path.display().to_string();
    let orig = openclaw_path.display().to_string();
    let mut lines = vec![
        "If anything goes wrong, restore from the backup:".to_string(),
        String::new(),
        format!("  $ sudo chmod 600 {bak}"),
        format!("  $ sudo cp {bak} {orig}"),
        format!("  $ sudo chmod 600 {orig}"),
    ];

    // Check if numbered backups exist and explain the scheme
    let bak1 = openclaw_path.with_file_name("openclaw.json.clawshell.bak.1");
    if bak1.exists() {
        lines.push(String::new());
        lines.push("Multiple backups exist — higher numbers are more recent.".to_string());
        lines.push("All backups are owned by 'clawshell' with mode 000 (no access).".to_string());
        lines.push("OpenClaw cannot read them — use sudo to restore.".to_string());
    }

    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    tui::print_callout("Recovery", &line_refs);
}

fn preview_matching_openclaw_config_env_removals(
    openclaw_path: &Path,
    mapped_real_key: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    if !openclaw_path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(openclaw_path)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;
    let Some(env) = json.get("env").and_then(serde_json::Value::as_object) else {
        return Ok(Vec::new());
    };

    let mut removals: Vec<String> = env
        .iter()
        .filter_map(|(key, value)| {
            let is_legacy = matches!(
                key.as_str(),
                "OPENAI_API_KEY" | "ANTHROPIC_API_KEY" | "ANTHROPIC_OAUTH_TOKEN"
            );
            if !is_legacy {
                return None;
            }
            value
                .as_str()
                .is_some_and(|v| v.trim() == mapped_real_key)
                .then(|| format!("env.{key}"))
        })
        .collect();
    removals.sort_unstable();
    Ok(removals)
}

#[derive(Debug, Clone)]
struct OpenclawConfigMutationPreview {
    env_after: serde_json::Value,
    default_models_after: serde_json::Value,
    providers_after: serde_json::Value,
    env_removals: Vec<String>,
}

fn json_object_at_pointer_or_empty(json: &serde_json::Value, pointer: &str) -> serde_json::Value {
    json.pointer(pointer)
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

fn build_openclaw_config_mutation_preview(
    openclaw_path: &Path,
    ob_config: &onboard::OnboardConfig,
) -> Result<OpenclawConfigMutationPreview, Box<dyn Error>> {
    let current_json = if openclaw_path.exists() {
        let content = std::fs::read_to_string(openclaw_path)?;
        serde_json::from_str::<serde_json::Value>(&content)?
    } else {
        serde_json::json!({})
    };

    let partial_json = serde_json::json!({
        "env": json_object_at_pointer_or_empty(&current_json, "/env"),
        "agents": {
            "defaults": {
                "models": json_object_at_pointer_or_empty(&current_json, "/agents/defaults/models")
            }
        },
        "models": {
            "providers": json_object_at_pointer_or_empty(&current_json, "/models/providers")
        }
    });
    let partial_content = serde_json::to_string(&partial_json)?;
    let modified_content =
        onboard::patch_openclaw_config_for_clawshell(&partial_content, ob_config)?;
    let modified_json: serde_json::Value = serde_json::from_str(&modified_content)?;

    Ok(OpenclawConfigMutationPreview {
        env_after: json_object_at_pointer_or_empty(&modified_json, "/env"),
        default_models_after: json_object_at_pointer_or_empty(
            &modified_json,
            "/agents/defaults/models",
        ),
        providers_after: json_object_at_pointer_or_empty(&modified_json, "/models/providers"),
        env_removals: preview_matching_openclaw_config_env_removals(
            openclaw_path,
            &ob_config.real_api_key,
        )?,
    })
}

fn fallback_openclaw_config_mutation_preview(
    ob_config: &onboard::OnboardConfig,
) -> OpenclawConfigMutationPreview {
    let model_key = format!("clawshell/{}", ob_config.model);
    let base_url = format!(
        "http://{}:{}/v1",
        ob_config.server_host, ob_config.server_port
    );
    OpenclawConfigMutationPreview {
        env_after: serde_json::json!({
            "CLAWSHELL_API_KEY": ob_config.virtual_api_key
        }),
        default_models_after: serde_json::json!({
            model_key: {
                "alias": "clawshell"
            }
        }),
        providers_after: serde_json::json!({
            "clawshell": {
                "baseUrl": base_url,
                "api": "openai-completions",
                "apiKey": "${CLAWSHELL_API_KEY}",
                "models": [
                    {
                        "id": ob_config.model,
                        "name": ob_config.model,
                    }
                ]
            }
        }),
        env_removals: Vec::new(),
    }
}

fn print_openclaw_config_mutation_preview(
    preview: &OpenclawConfigMutationPreview,
) -> Result<(), Box<dyn Error>> {
    let print_json = |label: &str, value: &serde_json::Value| -> Result<(), Box<dyn Error>> {
        tui::print_info(label, "");
        let pretty = serde_json::to_string_pretty(value)?;
        for line in pretty.lines() {
            println!("    {line}");
        }
        Ok(())
    };

    print_json("Set env", &preview.env_after)?;
    print_json("Set agents.defaults.models", &preview.default_models_after)?;
    print_json("Set models.providers", &preview.providers_after)?;
    if preview.env_removals.is_empty() {
        tui::print_info(
            "Remove from config",
            "none (no mapped legacy env key in openclaw.json)",
        );
    } else {
        for removal in &preview.env_removals {
            tui::print_info("Remove from config", removal);
        }
    }
    Ok(())
}

fn print_openclaw_cleanup_file_preview(preview: &onboard::OpenclawFileRemovalPreview) {
    tui::print_info("Edit file", &preview.path.display().to_string());
    for removal in &preview.removals {
        tui::print_info("Remove", removal);
    }
    tui::print_info("Backup", &preview.backup_path.display().to_string());
}

fn ensure_service_installed_for_lifecycle() -> Result<(), Box<dyn Error>> {
    if platform::service_exists()? {
        return Ok(());
    }

    Err(format!(
        "ClawShell service is not installed at '{}'. Run 'sudo clawshell onboard' to install it.",
        onboard::autostart_service_path()
    )
    .into())
}

fn ensure_service_config_matches(requested_config: &Path) -> Result<(), Box<dyn Error>> {
    ensure_service_installed_for_lifecycle()?;

    let configured = platform::service_config_path()?.ok_or_else(|| {
        format!(
            "Could not determine service config path from '{}'. Reinstall the service with 'sudo clawshell onboard'.",
            onboard::autostart_service_path()
        )
    })?;

    if paths_equivalent(&configured, requested_config) {
        return Ok(());
    }

    Err(format!(
        "Service is configured with '{}', but command requested '{}'. Reinstall the auto-start service to change the config path.",
        configured.display(),
        requested_config.display()
    )
    .into())
}

fn print_migration_status(path: &str, status: &VersionGateStatus) {
    match status {
        VersionGateStatus::Current(version) => {
            tui::print_info("Schema version", &version.to_string());
        }
        VersionGateStatus::Missing => {
            tui::print_warning("Schema version: missing (migration required)");
            tui::print_info("Run", &format!("clawshell migrate-config --config {path}"));
        }
        VersionGateStatus::Mismatch { found } => {
            tui::print_warning(&format!(
                "Schema version mismatch: found {}, expected {}",
                found,
                crate::migration::core::ConfigVersion::current()
            ));
            tui::print_info("Run", &format!("clawshell migrate-config --config {path}"));
        }
    }
}

fn ensure_rustls_crypto_provider() -> Result<(), Box<dyn Error>> {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return Ok(());
    }

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| std::io::Error::other("failed to install rustls ring CryptoProvider"))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ensure_rustls_crypto_provider()?;

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { config, foreground } => cmd_start(&config, foreground).await?,
        Commands::Stop => cmd_stop()?,
        Commands::Status => cmd_status()?,
        Commands::Restart { config } => cmd_restart(&config).await?,
        Commands::Logs {
            level,
            filter,
            num,
            follow,
        } => cmd_logs(level, filter, num, follow).await?,
        Commands::Config { config, edit } => cmd_config(&config, edit)?,
        Commands::MigrateConfig {
            config,
            on_ambiguous,
        } => cmd_migrate_config(&config, on_ambiguous)?,
        Commands::Onboard => cmd_onboard()?,
        Commands::Uninstall { yes } => cmd_uninstall(yes)?,
        Commands::Version => cmd_version(),
    }

    Ok(())
}

async fn cmd_start(config_path: &str, foreground: bool) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Start");
    if foreground {
        return cmd_start_inner(config_path).await;
    }

    let path = PathBuf::from(config_path);
    ensure_config_migrated(&path)?;
    Config::from_file(&path)
        .map_err(|e| format!("Failed to load configuration from '{}': {}", config_path, e))?;
    ensure_service_config_matches(&path)?;

    tui::print_info("Config", config_path);
    println!("Starting ClawShell via service manager...");
    platform::service_start()?;
    tui::print_success("ClawShell started successfully.");
    tui::print_info("Logs", &process::log_file_path().display().to_string());
    Ok(())
}

async fn cmd_start_inner(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Validate configuration
    let path = PathBuf::from(config_path);
    ensure_config_migrated(&path)?;
    let config = Config::from_file(&path)
        .map_err(|e| format!("Failed to load configuration from '{}': {}", config_path, e))?;
    let app_state = AppState::from_config(&config)
        .map_err(|e| format!("Failed to initialize app state: {e}"))?;

    tui::print_success("Configuration validated successfully.");

    // Ensure runtime directories exist (for log files)
    process::ensure_runtime_dirs()?;

    let env_filter: tracing_subscriber::EnvFilter = config
        .log_level
        .parse()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .init();

    info!(
        listen = config.listen_addr(),
        upstream = config.upstream.openai_base_url,
        keys = config.keys.len(),
        "ClawShell starting"
    );
    debug!(
        dlp_patterns = config.dlp.patterns.len(),
        scan_responses = config.dlp.scan_responses,
        log_level = %config.log_level,
        "Configuration loaded"
    );

    let addr: SocketAddr = config.listen_addr().parse()?;
    let app = build_router(app_state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Listening on {}", addr);

    process::drop_privileges()?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let ctrl_c = signal::ctrl_c();
            #[cfg(unix)]
            let mut term = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();
            #[cfg(unix)]
            tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} };
            #[cfg(not(unix))]
            ctrl_c.await.ok();
            info!("Shutdown signal received");
        })
        .await?;

    info!("ClawShell shut down");
    Ok(())
}

fn cmd_stop() -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Stop");
    ensure_default_config_migrated_if_present()?;
    ensure_service_installed_for_lifecycle()?;

    if !platform::service_is_running()? {
        tui::print_warning("ClawShell is not running.");
        return Ok(());
    }

    println!("Stopping ClawShell via service manager...");
    platform::service_stop()?;
    tui::print_success("ClawShell stopped successfully.");
    tui::print_info("Logs", &process::log_file_path().display().to_string());
    Ok(())
}

fn cmd_status() -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Status");
    ensure_service_installed_for_lifecycle()?;

    if platform::service_is_running()? {
        tui::print_success("ClawShell is running.");
    } else {
        tui::print_warning("ClawShell is not running.");
    }
    Ok(())
}

async fn cmd_restart(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Restart");
    let path = PathBuf::from(config_path);
    ensure_config_migrated(&path)?;
    Config::from_file(&path)
        .map_err(|e| format!("Failed to load configuration from '{}': {}", config_path, e))?;
    ensure_service_config_matches(&path)?;

    tui::print_info("Config", config_path);
    println!("Restarting ClawShell via service manager...");
    platform::service_restart()?;
    tui::print_success("ClawShell restarted successfully.");
    tui::print_info("Logs", &process::log_file_path().display().to_string());
    Ok(())
}

async fn cmd_logs(
    level: Option<String>,
    filter: Option<String>,
    num: usize,
    follow: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Logs");

    let log_path = process::log_file_path();

    if !log_path.exists() {
        tui::print_warning(&format!(
            "No logs available. Log file not found at: {}",
            log_path.display()
        ));
        return Ok(());
    }

    let file = std::fs::File::open(&log_path)?;
    let reader = BufReader::new(file);

    let lines: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .filter(|line| {
            if let Some(ref lvl) = level {
                let lvl_upper = lvl.to_uppercase();
                line.to_uppercase().contains(&lvl_upper)
            } else {
                true
            }
        })
        .filter(|line| {
            if let Some(ref keyword) = filter {
                line.contains(keyword)
            } else {
                true
            }
        })
        .collect();

    if lines.is_empty() {
        tui::print_warning("No matching log entries found.");
        return Ok(());
    }

    // Show last `num` lines
    let start = if lines.len() > num {
        lines.len() - num
    } else {
        0
    };

    for line in &lines[start..] {
        println!("{}", line);
    }

    if follow {
        tui::print_section("Following log output (Ctrl+C to stop)");
        // Use tail -f approach: read new lines as they appear
        let mut pos = std::fs::metadata(&log_path)?.len();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let current_len = std::fs::metadata(&log_path)?.len();
            if current_len > pos {
                let mut file = std::fs::File::open(&log_path)?;
                std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(pos))?;
                let reader = BufReader::new(file);
                for line in reader.lines().map_while(Result::ok) {
                    let show = level
                        .as_ref()
                        .is_none_or(|lvl| line.to_uppercase().contains(&lvl.to_uppercase()));
                    let show = show && filter.as_ref().is_none_or(|kw| line.contains(kw));
                    if show {
                        println!("{}", line);
                    }
                }
                pos = current_len;
            }
        }
    }

    Ok(())
}

fn cmd_config(config_path: &str, edit: bool) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Configuration");

    let path = PathBuf::from(config_path);

    if edit {
        if !path.exists() {
            tui::print_error(&format!("Configuration file not found: {config_path}"));
            std::process::exit(1);
        }

        ensure_config_migrated(&path)?;

        // Open in editor
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

        // Validate before editing to show current state
        if path.exists()
            && let Err(e) = Config::from_file(&path)
        {
            tui::print_warning(&format!("Current configuration has errors: {e}"));
        }

        let status = std::process::Command::new(&editor)
            .arg(config_path)
            .status()?;

        if !status.success() {
            tui::print_error("Editor exited with non-zero status.");
            return Ok(());
        }

        // Validate after editing
        match Config::from_file(&path) {
            Ok(config) => {
                tui::print_success("Configuration is valid.");
                tui::print_info("Server", &config.listen_addr());
                tui::print_info("Keys", &config.keys.len().to_string());
                tui::print_info("DLP patterns", &config.dlp.patterns.len().to_string());
                tui::print_warning(
                    "Changes will take effect after restarting ClawShell (clawshell restart).",
                );
            }
            Err(e) => {
                tui::print_error(&format!("Configuration validation failed: {e}"));
                tui::print_warning("Please fix the errors before restarting ClawShell.");
            }
        }

        return Ok(());
    }

    // Display mode
    if !path.exists() {
        tui::print_error(&format!("Configuration file not found: {config_path}"));
        tui::print_warning(&format!(
            "Run 'clawshell config --edit -f {}' to create one.",
            config_path
        ));
        std::process::exit(1);
    }

    let content = std::fs::read_to_string(&path)?;
    let version_status = clawshell_toml::version_gate_status(&path);

    // Validate
    match Config::from_file(&path) {
        Ok(config) => {
            tui::print_info("File", config_path);
            tui::print_success("Status: Valid");
            match &version_status {
                Ok(status) => print_migration_status(config_path, status),
                Err(e) => tui::print_warning(&format!(
                    "Could not determine migration status from version field: {}",
                    e
                )),
            }
            println!();

            tui::print_section("Server");
            tui::print_info("Listen", &config.listen_addr());
            tui::print_info("Log level", &config.log_level);
            tui::print_info("Upstream (OpenAI)", &config.upstream.openai_base_url);
            tui::print_info(
                "Upstream (OpenRouter)",
                config
                    .upstream
                    .openrouter_base_url
                    .as_deref()
                    .unwrap_or("https://openrouter.ai/api (default)"),
            );
            tui::print_info(
                "Upstream (Anthropic)",
                config
                    .upstream
                    .anthropic_base_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com (default)"),
            );

            tui::print_section("Keys");
            println!("  {} configured", config.keys.len());
            for key in &config.keys {
                println!(
                    "  {} {} (provider: {:?})",
                    tui::theme_style().apply_to("▸"),
                    key.virtual_key,
                    key.provider,
                );
            }

            tui::print_section("DLP");
            tui::print_info("Patterns", &config.dlp.patterns.len().to_string());
            tui::print_info(
                "Response scanning",
                if config.dlp.scan_responses {
                    "enabled"
                } else {
                    "disabled"
                },
            );
            for p in &config.dlp.patterns {
                println!(
                    "  {} {} (action: {:?})",
                    tui::theme_style().apply_to("▸"),
                    p.name,
                    p.action
                );
            }
        }
        Err(e) => {
            tui::print_info("File", config_path);
            tui::print_error(&format!("Status: INVALID - {e}"));
            if let Ok(status) = &version_status {
                print_migration_status(config_path, status);
            }
            println!();
            tui::print_section("Raw content");
            println!("{}", content);
        }
    }

    Ok(())
}

fn cmd_migrate_config(
    config_path: &str,
    on_ambiguous: Option<OnAmbiguousOption>,
) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Migrate Config");

    let path = PathBuf::from(config_path);
    if !path.exists() {
        tui::print_error(&format!("Configuration file not found: {}", path.display()));
        std::process::exit(1);
    }

    let targets: Vec<Box<dyn MigrationTarget>> = vec![Box::new(ClawshellTomlTarget::new(path))];
    let mut resolver: Box<dyn AmbiguityResolver> = match on_ambiguous {
        Some(OnAmbiguousOption::Fail) => Box::new(FailOnAmbiguousResolver),
        None => Box::new(InteractiveAmbiguityResolver),
    };

    let report = orchestrator::migrate_targets(&targets, resolver.as_mut())?;
    tui::print_info("Target version", &report.to_version.to_string());

    for target in report.targets {
        println!();
        tui::print_section(&format!("Target: {}", target.target_name));
        tui::print_info("File", &target.path.display().to_string());
        tui::print_info("From", &target.from_version.to_string());
        tui::print_info("To", &target.to_version.to_string());
        if target.changed {
            tui::print_success("Migration applied.");
            if let Some(backup) = target.backup_path {
                tui::print_info("Backup", &backup.display().to_string());
            }
        } else {
            tui::print_success("Already up to date.");
        }

        for step in target.applied_steps {
            tui::print_info("Step", &step);
        }
        for warning in target.warnings {
            tui::print_warning(&warning);
        }
    }

    println!();
    tui::print_success("Migration completed.");
    Ok(())
}

fn cmd_onboard() -> Result<(), Box<dyn std::error::Error>> {
    use crate::onboard;

    const TOTAL_STEPS: usize = 9;

    tui::print_banner("Onboarding");

    // Check if running as root
    if !nix::unistd::getuid().is_root() {
        tui::print_callout(
            "Administrative Privileges Required",
            &[
                "This process needs to set secure permissions on sensitive",
                "files such as API keys and configuration.",
                "",
                "Please re-run with sudo:",
                "",
                "  $ sudo clawshell onboard",
            ],
        );
        std::process::exit(1);
    }

    let existing_config = process::default_config_path();
    if existing_config.exists() {
        ensure_config_migrated(&existing_config)?;
    }

    tui::print_warning("Administrative privileges in use — securing sensitive files.");
    println!();

    // Step 1: Create the clawshell user if it doesn't exist
    tui::print_step(1, TOTAL_STEPS, "Checking for 'clawshell' system user...");

    let user_exists = std::process::Command::new("id")
        .arg("clawshell")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if user_exists {
        tui::print_step_done(1, TOTAL_STEPS, "System user already exists");
    } else {
        if let Err(error) = platform::create_system_user("clawshell") {
            tui::print_error(&format!("Failed to create 'clawshell' user: {error}"));
            std::process::exit(1);
        }
        tui::print_step_done(1, TOTAL_STEPS, "System user created");
    }

    // Step 2: Create necessary directories
    let config_dir = PathBuf::from("/etc/clawshell");
    let log_dir_path = PathBuf::from("/var/log/clawshell");

    tui::print_step(2, TOTAL_STEPS, "Setting up directories...");

    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&log_dir_path)?;
    tui::print_step_done(2, TOTAL_STEPS, "Directories created");

    // Step 3: Set permissions and ownership
    tui::print_step(3, TOTAL_STEPS, "Setting permissions and ownership...");

    if let Err(error) = platform::set_mode(&config_dir, 0o700) {
        warn!(
            error = %error,
            path = %config_dir.display(),
            "Failed to set config directory permissions"
        );
    }
    if let Err(error) = platform::set_owner(&config_dir, true) {
        warn!(
            error = %error,
            path = %config_dir.display(),
            "Failed to set config directory owner"
        );
    }
    if let Err(error) = platform::set_owner(&log_dir_path, true) {
        warn!(
            error = %error,
            path = %log_dir_path.display(),
            "Failed to set log directory owner"
        );
    }
    tui::print_step_done(3, TOTAL_STEPS, "Permissions set");

    // Step 4: Ask the user for configuration details (TUI prompts)
    tui::print_step(4, TOTAL_STEPS, "Collecting configuration...");
    println!(); // newline before interactive prompts

    let ob_config = onboard::collect_onboard_config_tui()?;

    tui::print_step_done(4, TOTAL_STEPS, "Configuration collected");

    // Step 5: Write the ClawShell configuration file
    tui::print_step(5, TOTAL_STEPS, "Writing ClawShell configuration...");

    let config_file = config_dir.join("config.json");
    let toml_config_path = config_dir.join("clawshell.toml");
    let toml_content = onboard::generate_clawshell_config(&ob_config);
    std::fs::write(&toml_config_path, &toml_content)?;

    let config_json = serde_json::json!({
        "real_api_key": ob_config.real_api_key,
        "virtual_api_key": ob_config.virtual_api_key,
        "provider": ob_config.provider,
        "model": ob_config.model,
        "openclaw_config_path": ob_config.openclaw_config_path.to_string_lossy(),
    });
    std::fs::write(&config_file, serde_json::to_string_pretty(&config_json)?)?;

    // Set permissions on config files
    if let Err(error) = platform::set_mode(&config_file, 0o600) {
        warn!(
            error = %error,
            path = %config_file.display(),
            "Failed to set config.json permissions"
        );
    }
    if let Err(error) = platform::set_mode(&toml_config_path, 0o600) {
        warn!(
            error = %error,
            path = %toml_config_path.display(),
            "Failed to set clawshell.toml permissions"
        );
    }
    if let Err(error) = platform::set_owner(&config_file, false) {
        warn!(
            error = %error,
            path = %config_file.display(),
            "Failed to set config.json owner"
        );
    }
    if let Err(error) = platform::set_owner(&toml_config_path, false) {
        warn!(
            error = %error,
            path = %toml_config_path.display(),
            "Failed to set clawshell.toml owner"
        );
    }
    tui::print_step_done(5, TOTAL_STEPS, "Configuration written");

    // Step 6: Write OpenClaw skill files
    tui::print_step(6, TOTAL_STEPS, "OpenClaw skill setup...");
    println!();
    let openclaw_skill_edit_approved =
        tui::prompt_confirm("Write OpenClaw skill files for email integration", true)?;
    let openclaw_skill = if openclaw_skill_edit_approved {
        write_onboard_openclaw_skill(&ob_config)?
    } else {
        None
    };
    if openclaw_skill_edit_approved {
        if let Some(skill) = openclaw_skill.as_ref() {
            onboard::upsert_managed_skill_manifest_entry(&config_file, &skill.manifest_entry)?;
            tui::print_step_done(6, TOTAL_STEPS, "OpenClaw skills written");
            tui::print_info("OpenClaw skill", &skill.path.display().to_string());
        } else {
            tui::print_step_done(6, TOTAL_STEPS, "OpenClaw skills skipped");
        }
    } else {
        tui::print_step_done(
            6,
            TOTAL_STEPS,
            "OpenClaw skills skipped (approval not granted)",
        );
    }

    // OpenClaw config path was already asked in step 4
    let openclaw_path = &ob_config.openclaw_config_path;

    // Step 7: Backup OpenClaw configuration file if present.
    tui::print_step(7, TOTAL_STEPS, "Backing up OpenClaw configuration...");
    if openclaw_path.exists() {
        let backup = onboard::backup_openclaw_config(openclaw_path)?;
        tui::print_step_done(7, TOTAL_STEPS, "OpenClaw config backed up");
        tui::print_info("Backup", &backup.display().to_string());
        print_openclaw_recovery_notice(openclaw_path, &backup);
    } else {
        tui::print_step_done(7, TOTAL_STEPS, "OpenClaw config backup skipped");
        tui::print_warning(&format!(
            "OpenClaw config not found at: {}",
            openclaw_path.display()
        ));
    }

    // Step 8: Remove legacy provider credentials and update OpenClaw config.
    tui::print_step(8, TOTAL_STEPS, "OpenClaw update setup...");
    let openclaw_state_dir = onboard::openclaw_config_root(openclaw_path);
    println!();
    let cleanup_preview = onboard::preview_openclaw_provider_credential_cleanup(
        &openclaw_state_dir,
        &ob_config.real_api_key,
    )?;
    let config_mutation_preview = match build_openclaw_config_mutation_preview(
        openclaw_path,
        &ob_config,
    ) {
        Ok(preview) => preview,
        Err(error) => {
            warn!(
                error = %error,
                path = %openclaw_path.display(),
                "Failed to build exact OpenClaw config mutation preview; showing fallback payload"
            );
            fallback_openclaw_config_mutation_preview(&ob_config)
        }
    };
    tui::print_info(
        "OpenClaw state dir",
        &openclaw_state_dir.display().to_string(),
    );
    tui::print_info("OpenClaw config path", &openclaw_path.display().to_string());
    tui::print_info(
        "Mapped-key policy",
        "Only entries matching the mapped virtual-key target will be removed",
    );
    if cleanup_preview.state_dir_exists {
        if let Some(dot_env) = cleanup_preview.dot_env.as_ref() {
            print_openclaw_cleanup_file_preview(dot_env);
        }
        for auth_profile in &cleanup_preview.auth_profiles {
            print_openclaw_cleanup_file_preview(auth_profile);
        }
        if let Some(oauth) = cleanup_preview.oauth.as_ref() {
            print_openclaw_cleanup_file_preview(oauth);
        }
        if !cleanup_preview.has_changes() {
            tui::print_info("State-dir edits", "none (no mapped-key match)");
        }
    } else {
        tui::print_warning(&format!(
            "OpenClaw state dir not found: {}",
            openclaw_state_dir.display()
        ));
    }
    print_openclaw_config_mutation_preview(&config_mutation_preview)?;
    let openclaw_edit_approved = tui::prompt_confirm(
        "Proceed with the exact OpenClaw edits shown above (backups first)",
        true,
    )?;
    if !openclaw_edit_approved {
        tui::print_step_done(
            8,
            TOTAL_STEPS,
            "OpenClaw update skipped (approval not granted)",
        );
        return Err("Onboarding aborted: OpenClaw edit approval was not granted.".into());
    }

    tui::print_step(8, TOTAL_STEPS, "Applying OpenClaw updates...");
    // Step status renders inline; break once before interactive OpenClaw approvals.
    println!();
    let cleanup = onboard::cleanup_openclaw_provider_credentials(
        &openclaw_state_dir,
        &ob_config.real_api_key,
    )?;
    if cleanup.has_changes() {
        tui::print_info("Legacy credential cleanup", "applied");
        tui::print_info(
            "Env entries removed",
            &cleanup.dot_env_entries_removed.to_string(),
        );
        tui::print_info(
            "Auth profiles updated",
            &cleanup.auth_profile_files_updated.to_string(),
        );
        tui::print_info(
            "Auth profile entries removed",
            &cleanup.auth_profile_entries_removed.to_string(),
        );
        tui::print_info(
            "OAuth entries removed",
            &cleanup.oauth_entries_removed.to_string(),
        );
        tui::print_info(
            "Backup files created",
            &cleanup.backup_files_created.to_string(),
        );
    }
    let mut openclaw_runner = openclaw_cli::RealOpenclawRunner;
    openclaw_cli::apply_onboard_openclaw_config(&mut openclaw_runner, &ob_config)?;
    if let Some(skill) = openclaw_skill.as_ref() {
        align_owner_with_openclaw_path(&skill.path, openclaw_path)?;
    }
    tui::print_step_done(8, TOTAL_STEPS, "OpenClaw config updated");

    // Auto-start service setup (ask before step 9 so we can start via service manager)
    let exe = std::env::current_exe()?;
    let service_path = std::path::Path::new(onboard::autostart_service_path());
    let service_exists = service_path.exists();
    let prompt_msg = if service_exists {
        "Auto-start service already exists. Reinstall it?"
    } else {
        "Install auto-start service so ClawShell starts on boot?"
    };
    let installed_service = tui::prompt_confirm(prompt_msg, !service_exists).unwrap_or(false);
    if installed_service {
        match onboard::install_autostart_service(&exe, &toml_config_path) {
            Ok(()) => {
                tui::print_success("Auto-start service installed.");
                tui::print_info("Service", onboard::autostart_service_path());
            }
            Err(e) => {
                tui::print_error(&format!("Failed to install auto-start service: {e}"));
            }
        }
    } else {
        tui::print_info(
            "Skipped",
            &format!(
                "You can install later by placing a service file at: {}",
                onboard::autostart_service_path()
            ),
        );
    }

    // Step 9: Start or skip ClawShell
    let already_running = platform::service_is_running().unwrap_or(false);

    if already_running {
        tui::print_step_done(9, TOTAL_STEPS, "ClawShell already running (skipped)");
    } else {
        tui::print_step(9, TOTAL_STEPS, "Starting ClawShell...");

        if installed_service {
            // Start via the service manager so it manages the lifecycle
            match onboard::start_autostart_service() {
                Ok(()) => {
                    tui::print_step_done(9, TOTAL_STEPS, "ClawShell started via service manager");
                }
                Err(e) => {
                    tui::print_error(&format!("Failed to start via service manager: {e}"));
                }
            }
        } else {
            start_clawshell_direct(&toml_config_path)?;
            tui::print_step_done(9, TOTAL_STEPS, "ClawShell started");
        }
        tui::print_info("Logs", &process::log_file_path().display().to_string());
    }

    // Summary
    tui::print_section("Setup Summary");
    tui::print_info("Provider", &ob_config.provider);
    tui::print_info("Model", &ob_config.model);
    tui::print_info("Virtual Key", &ob_config.virtual_api_key);
    tui::print_info(
        "Email",
        if ob_config.email.is_some() {
            "configured"
        } else {
            "not configured"
        },
    );
    tui::print_info(
        "Server",
        &format!("http://{}:{}", ob_config.server_host, ob_config.server_port),
    );
    tui::print_info("Config", &toml_config_path.display().to_string());
    tui::print_info("OpenClaw", &openclaw_path.display().to_string());
    println!();
    if already_running {
        tui::print_success("ClawShell configuration updated.");
    } else {
        tui::print_success("ClawShell is installed and running.");
    }
    if already_running {
        let restart_self = tui::prompt_confirm(
            "ClawShell is already running. Run `sudo clawshell restart` to apply the new configuration?",
            true,
        )
        .unwrap_or(false);

        if restart_self {
            let exe = std::env::current_exe()?;
            let status = std::process::Command::new("sudo")
                .args([exe.to_string_lossy().as_ref(), "restart"])
                .status();
            match status {
                Ok(s) if s.success() => tui::print_success("ClawShell restarted."),
                Ok(s) => tui::print_error(&format!(
                    "Failed to restart ClawShell (exit code {}).",
                    s.code().unwrap_or(-1)
                )),
                Err(e) => tui::print_error(&format!("Failed to run 'sudo clawshell restart': {e}")),
            }
        } else {
            tui::print_info(
                "Skipped",
                "You can restart later with: sudo clawshell restart",
            );
        }
    }

    // Ask whether to set default model to clawshell
    println!();
    let set_model = tui::prompt_confirm(
        "Run `openclaw models set clawshell` to set the default model to the ClawShell proxy?",
        true,
    )
    .unwrap_or(false);

    if set_model {
        let mut openclaw_runner = openclaw_cli::RealOpenclawRunner;
        match openclaw_cli::run_openclaw_command(
            &mut openclaw_runner,
            &["models", "set", "clawshell"],
        ) {
            Ok(output) if output.success => tui::print_success("Default model set to clawshell."),
            Ok(output) => tui::print_error(&format!(
                "Failed to set default model (exit code {}).",
                output.status_code.unwrap_or(-1)
            )),
            Err(error) => tui::print_error(&format!(
                "Failed to run 'openclaw models set clawshell': {error}"
            )),
        }
    } else {
        tui::print_info(
            "Skipped",
            "You can set it later with: openclaw models set clawshell",
        );
    }

    // Ask whether to restart the gateway
    let restart_gw = tui::prompt_confirm(
        "Run `openclaw gateway restart` to apply the new configuration?",
        true,
    )
    .unwrap_or(false);

    if restart_gw {
        let mut openclaw_runner = openclaw_cli::RealOpenclawRunner;
        match openclaw_cli::run_openclaw_command(&mut openclaw_runner, &["gateway", "restart"]) {
            Ok(output) if output.success => tui::print_success("OpenClaw gateway restarted."),
            Ok(output) => tui::print_error(&format!(
                "Failed to restart gateway (exit code {}).",
                output.status_code.unwrap_or(-1)
            )),
            Err(error) => tui::print_error(&format!(
                "Failed to run 'openclaw gateway restart': {error}"
            )),
        }
    } else {
        tui::print_info(
            "Skipped",
            "You can restart later with: openclaw gateway restart",
        );
    }

    Ok(())
}

fn cmd_uninstall(skip_confirm: bool) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Uninstall");

    if !nix::unistd::getuid().is_root() {
        tui::print_callout(
            "Administrative Privileges Required",
            &[
                "This process needs to remove secured files and the",
                "system user safely.",
                "",
                "Please re-run with sudo:",
                "",
                "  $ sudo clawshell uninstall",
            ],
        );
        std::process::exit(1);
    }

    ensure_default_config_migrated_if_present()?;

    tui::print_warning("Administrative privileges in use — removing secured files safely.");
    println!();

    let exe_path = std::env::current_exe()?;
    let config_dir = PathBuf::from(process::CONFIG_DIR);
    let log_dir = process::log_file_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/var/log/clawshell"));

    let service_path = std::path::Path::new(crate::onboard::autostart_service_path());
    let service_exists = service_path.exists();
    let clawshell_config_file = config_dir.join("config.json");
    let openclaw_path = if clawshell_config_file.exists() {
        try_read_openclaw_config_path(&clawshell_config_file)
    } else {
        None
    };
    let openclaw_skill_dir = openclaw_path.as_ref().map(|path| {
        onboard::openclaw_config_root(path)
            .join("skills")
            .join(onboard::OPENCLAW_EMAIL_MESSAGES_SKILL_NAME)
    });
    let openclaw_skill_manifest = if clawshell_config_file.exists() {
        onboard::read_managed_skill_manifest_entry(
            &clawshell_config_file,
            onboard::OPENCLAW_EMAIL_MESSAGES_SKILL_NAME,
        )
    } else {
        None
    };
    let openclaw_skill_inspection = if let Some(skill_dir) = openclaw_skill_dir.as_ref() {
        onboard::inspect_managed_skill_for_uninstall(
            skill_dir,
            onboard::OPENCLAW_EMAIL_MESSAGES_SKILL_NAME,
            openclaw_skill_manifest.as_ref(),
        )
    } else {
        onboard::ManagedSkillInspection::missing()
    };

    tui::print_warning("This will remove the following:");
    tui::print_info("ClawShell", "Stop if running");
    tui::print_info("Config dir", &config_dir.display().to_string());
    tui::print_info("Log dir", &log_dir.display().to_string());
    if service_exists {
        tui::print_info("Service", &service_path.display().to_string());
    }
    if let Some(skill_dir) = openclaw_skill_dir.as_ref()
        && skill_dir.exists()
    {
        match openclaw_skill_inspection.state {
            onboard::ManagedSkillUninstallState::ManagedUnchanged => {
                tui::print_info(
                    "OpenClaw skill",
                    &format!("{} (managed)", skill_dir.display()),
                );
            }
            onboard::ManagedSkillUninstallState::ManagedModified => {
                tui::print_info(
                    "OpenClaw skill",
                    &format!("{} (managed, modified)", skill_dir.display()),
                );
                tui::print_warning(&format!(
                    "Managed OpenClaw skill has local modifications: {}",
                    openclaw_skill_inspection.detail
                ));
            }
            onboard::ManagedSkillUninstallState::Unmanaged => {
                tui::print_info(
                    "OpenClaw skill",
                    &format!("{} (legacy/unverified)", skill_dir.display()),
                );
                tui::print_warning(&format!(
                    "Skill ownership is not verified: {}",
                    openclaw_skill_inspection.detail
                ));
            }
            onboard::ManagedSkillUninstallState::Missing => {}
        }
    }
    tui::print_info("Binary", &format!("{} (preserved)", exe_path.display()));
    tui::print_info("System user", "clawshell");
    println!();

    if !skip_confirm {
        let confirmed =
            tui::prompt_confirm("Are you sure you want to uninstall ClawShell?", false)?;
        if !confirmed {
            tui::print_warning("Uninstall cancelled.");
            return Ok(());
        }
        println!();
    }

    // 0. Clean up OpenClaw configuration (before any destructive operations)
    if let Some(openclaw_path) = openclaw_path.as_ref()
        && openclaw_path.exists()
    {
        tui::print_info("Action", "Cleaning up OpenClaw configuration...");
        let mut openclaw_runner = openclaw_cli::RealOpenclawRunner;
        match openclaw_cli::cleanup_openclaw_for_uninstall(&mut openclaw_runner)? {
            openclaw_cli::UninstallCleanupOutcome::BlockedByDefaultModel => {
                tui::print_error(
                    "ClawShell model is currently set as the default model in OpenClaw.",
                );
                tui::print_error(
                    "Please change the default model (for example, with `openclaw models set <model>`) before uninstalling.",
                );
                std::process::exit(1);
            }
            openclaw_cli::UninstallCleanupOutcome::Cleaned => {
                tui::print_success("OpenClaw configuration cleaned up.");
            }
        }
    }

    // 0b. Remove ClawShell-managed OpenClaw skill if present.
    if let Some(skill_dir) = openclaw_skill_dir.as_ref()
        && skill_dir.exists()
    {
        let remove_skill_dir = |path: &Path| match std::fs::remove_dir_all(path) {
            Ok(()) => tui::print_success(&format!("OpenClaw skill removed: {}", path.display())),
            Err(error) => tui::print_warning(&format!(
                "Failed to remove OpenClaw skill at {}: {error}",
                path.display()
            )),
        };

        match openclaw_skill_inspection.state {
            onboard::ManagedSkillUninstallState::ManagedUnchanged => {
                let remove_skill = if skip_confirm {
                    true
                } else {
                    let prompt =
                        format!("Remove managed OpenClaw skill at {}?", skill_dir.display());
                    tui::prompt_confirm(&prompt, true)?
                };

                if remove_skill {
                    remove_skill_dir(skill_dir);
                } else {
                    tui::print_info(
                        "Skipped",
                        &format!("OpenClaw skill preserved at {}", skill_dir.display()),
                    );
                }
            }
            onboard::ManagedSkillUninstallState::ManagedModified => {
                if skip_confirm {
                    tui::print_warning(&format!(
                        "Preserving managed-but-modified OpenClaw skill at {} (--yes does not force-delete modified skills).",
                        skill_dir.display()
                    ));
                } else {
                    let prompt = format!(
                        "Managed OpenClaw skill at {} was modified. Delete anyway?",
                        skill_dir.display()
                    );
                    let remove_skill = tui::prompt_confirm(&prompt, false)?;
                    if remove_skill {
                        remove_skill_dir(skill_dir);
                    } else {
                        tui::print_info(
                            "Skipped",
                            &format!("OpenClaw skill preserved at {}", skill_dir.display()),
                        );
                    }
                }
            }
            onboard::ManagedSkillUninstallState::Unmanaged => {
                if skip_confirm {
                    tui::print_warning(&format!(
                        "Preserving unverified OpenClaw skill at {} (--yes does not force-delete unverified skills).",
                        skill_dir.display()
                    ));
                } else {
                    let prompt = format!(
                        "No ClawShell ownership marker/manifest match at {}. Delete anyway? (high risk)",
                        skill_dir.display()
                    );
                    let remove_skill = tui::prompt_confirm(&prompt, false)?;
                    if remove_skill {
                        remove_skill_dir(skill_dir);
                    } else {
                        tui::print_info(
                            "Skipped",
                            &format!("OpenClaw skill preserved at {}", skill_dir.display()),
                        );
                    }
                }
            }
            onboard::ManagedSkillUninstallState::Missing => {}
        }
    }

    // 1. Stop ClawShell and remove auto-start service
    if service_exists {
        tui::print_info("Action", "Stopping and removing auto-start service...");
        match crate::onboard::remove_autostart_service() {
            Ok(()) => tui::print_success("Auto-start service stopped and removed."),
            Err(e) => tui::print_warning(&format!("Failed to remove auto-start service: {e}")),
        }
    }

    // 2. Remove log directory
    if log_dir.exists() {
        std::fs::remove_dir_all(&log_dir)?;
        tui::print_success("Log directory removed.");
    }

    // 3. Remove configuration directory
    if config_dir.exists() {
        std::fs::remove_dir_all(&config_dir)?;
        tui::print_success("Configuration directory removed.");
    }

    // 4. Remove the clawshell system user
    let user_exists = std::process::Command::new("id")
        .arg("clawshell")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if user_exists {
        if let Err(error) = platform::delete_system_user("clawshell") {
            tui::print_warning(&format!("Failed to remove system user: {error}"));
        } else {
            tui::print_success("System user removed.");
        }
    }

    // 5. Preserve the binary so users can still run clawshell later.
    if exe_path.exists() {
        tui::print_info("Binary", &format!("Preserved at {}", exe_path.display()));
    } else {
        tui::print_warning("Binary path no longer exists; skipping binary preservation check.");
    }

    println!();
    tui::print_success("ClawShell has been uninstalled.");
    Ok(())
}

fn cmd_version() {
    let version = env!("CARGO_PKG_VERSION");

    tui::print_banner(&format!("v{version}"));
    println!();

    println!("{}", tui::theme_bold().apply_to("Features:"));
    let bullet = tui::theme_style().apply_to("▸");
    println!("  {bullet} Virtual key to real key mapping");
    println!("  {bullet} Multi-provider support (OpenAI, OpenRouter, Anthropic)");
    println!("  {bullet} DLP scanning with block/redact actions");
    println!("  {bullet} Response PII scanning");
    println!("  {bullet} Streaming support (SSE pass-through)");
}

/// Start ClawShell directly by spawning a child process (no service manager).
fn start_clawshell_direct(
    toml_config_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let log_path = process::log_file_path();
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_stderr = log_file.try_clone()?;

    let child = std::process::Command::new(exe)
        .args([
            "start",
            "--config",
            &toml_config_path.to_string_lossy(),
            "--foreground",
        ])
        .stdout(log_file)
        .stderr(log_stderr)
        .stdin(std::process::Stdio::null())
        .spawn()?;

    let pid = child.id();
    tui::print_info("Process ID", &pid.to_string());
    Ok(())
}
