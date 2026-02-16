use clap::Parser;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::signal;
use tracing::{debug, info, warn};

use clawshell::cli::{Cli, Commands};
use clawshell::config::Config;
use clawshell::platform;
use clawshell::process;
use clawshell::tui;
use clawshell::{AppState, build_router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        Commands::Onboard => cmd_onboard()?,
        Commands::Uninstall { yes } => cmd_uninstall(yes)?,
        Commands::Version => cmd_version(),
    }

    Ok(())
}

async fn cmd_start(config_path: &str, foreground: bool) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Start");
    cmd_start_inner(config_path, foreground).await
}

async fn cmd_start_inner(
    config_path: &str,
    foreground: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check if already running (skip if PID file points to ourselves — daemon child)
    let my_pid = std::process::id();
    if let Some(pid) = process::read_pid_file() {
        if pid != my_pid && process::is_process_running(pid) {
            tui::print_error(&format!("ClawShell is already running (PID: {pid})"));
            std::process::exit(1);
        }
        if pid != my_pid {
            // Stale PID file
            process::remove_pid_file();
        }
    }

    // Validate configuration
    let path = PathBuf::from(config_path);
    let config = Config::from_file(&path)
        .map_err(|e| format!("Failed to load configuration from '{}': {}", config_path, e))?;

    tui::print_success("Configuration validated successfully.");

    // Ensure runtime directories exist (for PID and log files)
    process::ensure_runtime_dirs()?;

    if !foreground {
        // Daemonize: fork a child process
        use std::process::Command;

        let exe = std::env::current_exe()?;
        let log_path = process::log_file_path();
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let log_stderr = log_file.try_clone()?;

        let child = Command::new(exe)
            .args(["start", "--config", config_path, "--foreground"])
            .stdout(log_file)
            .stderr(log_stderr)
            .stdin(std::process::Stdio::null())
            .spawn()?;

        let pid = child.id();

        // Write PID file immediately so stop/restart can find the process
        process::write_pid_file(pid)?;

        tui::print_success(&format!("ClawShell started in background (PID: {pid})"));
        tui::print_info("Logs", &log_path.display().to_string());
        return Ok(());
    }

    // Foreground mode — write PID if not already recorded by the parent daemon
    let pid = std::process::id();
    if process::read_pid_file() != Some(pid) {
        process::write_pid_file(pid)?;
    }

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
        upstream = config.upstream.base_url,
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
    let app = build_router(AppState::from_config(&config));
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

    process::remove_pid_file();
    info!("ClawShell shut down");
    Ok(())
}

fn cmd_stop() -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Stop");

    match process::read_pid_file() {
        Some(pid) => {
            if !process::is_process_running(pid) {
                tui::print_warning(&format!(
                    "ClawShell is not running (stale PID file for PID: {pid})"
                ));
                process::remove_pid_file();
                return Ok(());
            }
            tui::print_info("PID", &pid.to_string());
            println!("Stopping ClawShell...");
            process::stop_process(pid)?;
            tui::print_success("ClawShell stopped successfully.");
            tui::print_info("Logs", &process::log_file_path().display().to_string());
        }
        None => {
            tui::print_warning("ClawShell is not running.");
        }
    }
    Ok(())
}

fn cmd_status() -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Status");

    match process::read_pid_file() {
        Some(pid) => {
            if process::is_process_running(pid) {
                tui::print_success(&format!("ClawShell is running (PID: {pid})"));
                if let Some(uptime) = process::get_process_uptime(pid) {
                    tui::print_info("Uptime", &uptime);
                }
            } else {
                tui::print_warning(&format!(
                    "ClawShell is not running (stale PID file for PID: {pid})"
                ));
                process::remove_pid_file();
            }
        }
        None => {
            tui::print_warning("ClawShell is not running.");
        }
    }
    Ok(())
}

async fn cmd_restart(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    tui::print_banner("Restart");

    // Stop if running
    if let Some(pid) = process::read_pid_file() {
        if process::is_process_running(pid) {
            tui::print_info("PID", &pid.to_string());
            println!("Stopping ClawShell...");
            process::stop_process(pid)?;
            tui::print_success("ClawShell stopped.");
            // Wait briefly for the OS to release the port
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        } else {
            process::remove_pid_file();
        }
    }

    // Start with latest config
    tui::print_info("Config", config_path);
    println!("Starting ClawShell...");
    cmd_start_inner(config_path, false).await
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

    // Validate
    match Config::from_file(&path) {
        Ok(config) => {
            tui::print_info("File", config_path);
            tui::print_success("Status: Valid");
            println!();

            tui::print_section("Server");
            tui::print_info("Listen", &config.listen_addr());
            tui::print_info("Log level", &config.log_level);
            tui::print_info("Upstream (OpenAI)", &config.upstream.base_url);
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
            println!();
            tui::print_section("Raw content");
            println!("{}", content);
        }
    }

    Ok(())
}

fn cmd_onboard() -> Result<(), Box<dyn std::error::Error>> {
    use clawshell::onboard;

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
        let status = platform::create_system_user("clawshell")?;
        if !status.success() {
            tui::print_error("Failed to create 'clawshell' user.");
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

    // Also create runtime dirs for PID files
    let pid_path = process::pid_file_path();
    if let Some(pid_parent) = pid_path.parent() {
        std::fs::create_dir_all(pid_parent)?;
    }
    tui::print_step_done(2, TOTAL_STEPS, "Directories created");

    // Step 3: Set permissions and ownership
    tui::print_step(3, TOTAL_STEPS, "Setting permissions and ownership...");

    let chown_spec = platform::clawshell_chown_spec();

    if let Err(e) = std::process::Command::new("chmod")
        .args(["0700", &config_dir.to_string_lossy()])
        .status()
    {
        warn!(path = %config_dir.display(), error = %e, "Failed to chmod config directory");
    }
    if let Err(e) = std::process::Command::new("chown")
        .args(["-R", chown_spec, &config_dir.to_string_lossy()])
        .status()
    {
        warn!(path = %config_dir.display(), error = %e, "Failed to chown config directory");
    }
    if let Err(e) = std::process::Command::new("chown")
        .args(["-R", chown_spec, &log_dir_path.to_string_lossy()])
        .status()
    {
        warn!(path = %log_dir_path.display(), error = %e, "Failed to chown log directory");
    }
    if let Some(pid_parent) = pid_path.parent()
        && let Err(e) = std::process::Command::new("chown")
            .args([chown_spec, &pid_parent.to_string_lossy()])
            .status()
    {
        warn!(path = %pid_parent.display(), error = %e, "Failed to chown PID directory");
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
    let _ = std::process::Command::new("chmod")
        .args(["0600", &config_file.to_string_lossy()])
        .status();
    let _ = std::process::Command::new("chmod")
        .args(["0600", &toml_config_path.to_string_lossy()])
        .status();
    let _ = std::process::Command::new("chown")
        .args([chown_spec, &config_file.to_string_lossy()])
        .status();
    let _ = std::process::Command::new("chown")
        .args([chown_spec, &toml_config_path.to_string_lossy()])
        .status();
    tui::print_step_done(5, TOTAL_STEPS, "Configuration written");

    // Step 6: OpenClaw config path was already asked in step 4
    let openclaw_path = &ob_config.openclaw_config_path;

    // Step 7 & 8: Backup and modify OpenClaw configuration
    let actual_backup_path;
    if openclaw_path.exists() {
        tui::print_step(7, TOTAL_STEPS, "Backing up OpenClaw configuration...");
        actual_backup_path = Some(onboard::backup_openclaw_config(openclaw_path)?);
        tui::print_step_done(7, TOTAL_STEPS, "OpenClaw config backed up");
        tui::print_info(
            "Backup",
            &actual_backup_path.as_ref().unwrap().display().to_string(),
        );

        tui::print_step(8, TOTAL_STEPS, "Updating OpenClaw configuration...");
        let openclaw_content = std::fs::read_to_string(openclaw_path)?;
        let modified_content = onboard::modify_openclaw_config(&openclaw_content, &ob_config)?;
        std::fs::write(openclaw_path, &modified_content)?;
        tui::print_step_done(8, TOTAL_STEPS, "OpenClaw config updated");
    } else {
        actual_backup_path = None;
        tui::print_warning(&format!(
            "OpenClaw config not found at: {}",
            openclaw_path.display()
        ));

        if let Some(parent) = openclaw_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        tui::print_step(8, TOTAL_STEPS, "Creating new OpenClaw configuration...");
        let modified_content = onboard::modify_openclaw_config("{}", &ob_config)?;
        std::fs::write(openclaw_path, &modified_content)?;
        tui::print_step_done(8, TOTAL_STEPS, "OpenClaw config created");
        tui::print_info("Path", &openclaw_path.display().to_string());
    }

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
    let already_running = process::read_pid_file().is_some_and(process::is_process_running);

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

    // Run openclaw commands as the original (non-root) user so that
    // openclaw config files are owned by the right user and readable.
    let sudo_user = std::env::var("SUDO_USER").ok();

    if set_model {
        let status = if let Some(ref user) = sudo_user {
            std::process::Command::new("sudo")
                .args(["-u", user, "openclaw", "models", "set", "clawshell"])
                .status()
        } else {
            std::process::Command::new("openclaw")
                .args(["models", "set", "clawshell"])
                .status()
        };
        match status {
            Ok(s) if s.success() => tui::print_success("Default model set to clawshell."),
            Ok(s) => tui::print_error(&format!(
                "Failed to set default model (exit code {}).",
                s.code().unwrap_or(-1)
            )),
            Err(e) => tui::print_error(&format!("Failed to run 'openclaw models set': {e}")),
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
        let status = if let Some(ref user) = sudo_user {
            std::process::Command::new("sudo")
                .args(["-u", user, "openclaw", "gateway", "restart"])
                .status()
        } else {
            std::process::Command::new("openclaw")
                .args(["gateway", "restart"])
                .status()
        };
        match status {
            Ok(s) if s.success() => tui::print_success("OpenClaw gateway restarted."),
            Ok(s) => tui::print_error(&format!(
                "Failed to restart gateway (exit code {}).",
                s.code().unwrap_or(-1)
            )),
            Err(e) => tui::print_error(&format!("Failed to run 'openclaw gateway restart': {e}")),
        }
    } else {
        tui::print_info(
            "Skipped",
            "You can restart later with: openclaw gateway restart",
        );
    }

    // Recovery instructions notice
    if let Some(ref backup_path) = actual_backup_path {
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
            lines.push(
                "All backups are owned by 'clawshell' with mode 000 (no access).".to_string(),
            );
            lines.push("OpenClaw cannot read them — use sudo to restore.".to_string());
        }
        let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        tui::print_callout("Recovery", &line_refs);
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

    tui::print_warning("Administrative privileges in use — removing secured files safely.");
    println!();

    let exe_path = std::env::current_exe()?;
    let config_dir = PathBuf::from(process::CONFIG_DIR);
    let log_dir = process::log_file_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/var/log/clawshell"));
    let pid_file = process::pid_file_path();

    let service_path = std::path::Path::new(clawshell::onboard::autostart_service_path());
    let service_exists = service_path.exists();

    tui::print_warning("This will remove the following:");
    tui::print_info("ClawShell", "Stop if running");
    tui::print_info("Config dir", &config_dir.display().to_string());
    tui::print_info("Log dir", &log_dir.display().to_string());
    tui::print_info("PID file", &pid_file.display().to_string());
    if service_exists {
        tui::print_info("Service", &service_path.display().to_string());
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
    let clawshell_config_file = config_dir.join("config.json");
    if clawshell_config_file.exists()
        && let Ok(config_content) = std::fs::read_to_string(&clawshell_config_file)
        && let Ok(config_json) = serde_json::from_str::<serde_json::Value>(&config_content)
        && let Some(openclaw_path_str) = config_json
            .get("openclaw_config_path")
            .and_then(|v| v.as_str())
    {
        let openclaw_path = PathBuf::from(openclaw_path_str);
        if openclaw_path.exists() {
            let openclaw_content = std::fs::read_to_string(&openclaw_path)?;

            // Guard: reject uninstall if clawshell is the default model
            if clawshell::onboard::is_clawshell_default_model(&openclaw_content)? {
                tui::print_error(
                    "ClawShell model is currently set as the default model in OpenClaw.",
                );
                tui::print_error(&format!(
                    "Please change the default model in {} before uninstalling.",
                    openclaw_path.display()
                ));
                std::process::exit(1);
            }

            // Remove clawshell entries from OpenClaw config
            tui::print_info("Action", "Cleaning up OpenClaw configuration...");
            let cleaned = clawshell::onboard::remove_openclaw_entries(&openclaw_content)?;
            std::fs::write(&openclaw_path, cleaned)?;
            tui::print_success("OpenClaw configuration cleaned up.");
        }
    }

    // 1. Stop ClawShell and remove auto-start service
    if service_exists {
        tui::print_info("Action", "Stopping and removing auto-start service...");
        match clawshell::onboard::remove_autostart_service() {
            Ok(()) => tui::print_success("Auto-start service stopped and removed."),
            Err(e) => tui::print_warning(&format!("Failed to remove auto-start service: {e}")),
        }
    }

    // 2. Stop ClawShell if still running (e.g. started without service manager)
    if let Some(pid) = process::read_pid_file() {
        if process::is_process_running(pid) {
            tui::print_info("PID", &pid.to_string());
            println!("Stopping ClawShell...");
            process::stop_process(pid)?;
            tui::print_success("ClawShell stopped.");
        } else {
            process::remove_pid_file();
        }
    }

    // 3. Remove PID file (in case stop_process didn't clean it)
    if pid_file.exists() {
        let _ = std::fs::remove_file(&pid_file);
        tui::print_success("PID file removed.");
    }
    // Also remove the PID parent directory if it's a clawshell-specific dir
    if let Some(pid_dir) = pid_file.parent()
        && pid_dir.ends_with("clawshell")
    {
        let _ = std::fs::remove_dir(pid_dir);
    }

    // 4. Remove log directory
    if log_dir.exists() {
        std::fs::remove_dir_all(&log_dir)?;
        tui::print_success("Log directory removed.");
    }

    // 5. Remove configuration directory
    if config_dir.exists() {
        std::fs::remove_dir_all(&config_dir)?;
        tui::print_success("Configuration directory removed.");
    }

    // 6. Remove the clawshell system user
    let user_exists = std::process::Command::new("id")
        .arg("clawshell")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if user_exists {
        let status = platform::delete_system_user("clawshell")?;
        if status.success() {
            tui::print_success("System user removed.");
        } else {
            tui::print_warning(&format!("Failed to remove user (exit code: {status})."));
        }
    }

    // 7. Preserve the binary so users can still run clawshell later.
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
    println!("  {bullet} Multi-provider support (OpenAI, Anthropic)");
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
    process::write_pid_file(pid)?;
    tui::print_info("PID", &pid.to_string());
    Ok(())
}
