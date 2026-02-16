use crate::process;

use clap::builder::Styles;
use clap::builder::styling::{Ansi256Color, AnsiColor, Color, Effects, Style};
use clap::{Parser, Subcommand, ValueEnum};

fn default_config_path() -> String {
    process::default_config_path()
        .to_string_lossy()
        .into_owned()
}

/// The banner line shown before every --help output.
const BANNER: &str =
    "\n  🦊🦀 \x1b[1;38;5;208mClawShell\x1b[0m  \x1b[38;5;208mSecuring OpenClaw\x1b[0m\n";

/// Build clap Styles using our theme color: xterm-256 color 208 ≈ RGB(236, 142, 65).
const fn cli_styles() -> Styles {
    const THEME: Option<Color> = Some(Color::Ansi256(Ansi256Color(208)));

    Styles::styled()
        .header(Style::new().fg_color(THEME).effects(Effects::BOLD))
        .usage(Style::new().fg_color(THEME).effects(Effects::BOLD))
        .literal(Style::new().fg_color(THEME))
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightBlack))))
        .valid(
            Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Green)))
                .effects(Effects::BOLD),
        )
        .invalid(
            Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Red)))
                .effects(Effects::BOLD),
        )
        .error(
            Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Red)))
                .effects(Effects::BOLD),
        )
}

#[derive(Parser)]
#[command(
    name = "clawshell",
    about = "The security middleware designed to strap onto the OpenClaw ecosystem.",
    version,
    styles = cli_styles(),
    before_help = BANNER,
    after_help = "EXAMPLES:\n  \
        clawshell start                       Start with default config\n  \
        clawshell start -c /etc/clawshell/clawshell.toml  Start with a custom config\n  \
        clawshell stop                        Stop ClawShell\n  \
        clawshell status                      Check if ClawShell is running\n  \
        clawshell restart                     Restart ClawShell\n  \
        clawshell logs --level error          Show only error logs\n  \
        clawshell logs --filter \"timeout\"     Filter logs by keyword\n  \
        clawshell config                      Display current configuration\n  \
        clawshell config --edit               Edit the configuration file\n  \
        clawshell migrate-config              Migrate configuration to current schema\n  \
        clawshell onboard                     Set up the clawshell system user\n  \
        clawshell uninstall                   Remove ClawShell from the system\n  \
        clawshell version                     Show version information"
)]
#[derive(Debug)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OnAmbiguousOption {
    Fail,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start ClawShell
    #[command(before_help = BANNER)]
    Start {
        /// Path to the configuration file
        #[arg(short, long, default_value_t = default_config_path())]
        config: String,

        /// Run in the foreground (don't daemonize)
        #[arg(short, long)]
        foreground: bool,
    },

    /// Stop ClawShell
    #[command(before_help = BANNER)]
    Stop,

    /// Check the status of ClawShell
    #[command(before_help = BANNER)]
    Status,

    /// Restart ClawShell
    #[command(before_help = BANNER)]
    Restart {
        /// Path to the configuration file
        #[arg(short, long, default_value_t = default_config_path())]
        config: String,
    },

    /// View ClawShell logs
    #[command(before_help = BANNER)]
    Logs {
        /// Filter by log level (trace, debug, info, warn, error)
        #[arg(short, long)]
        level: Option<String>,

        /// Filter logs by keyword
        #[arg(short, long)]
        filter: Option<String>,

        /// Number of lines to show (default: 50)
        #[arg(short, long, default_value = "50")]
        num: usize,

        /// Follow log output (like tail -f)
        #[arg(long)]
        follow: bool,
    },

    /// View and edit the ClawShell configuration
    #[command(before_help = BANNER)]
    Config {
        /// Path to the configuration file
        #[arg(short = 'f', long = "file", default_value_t = default_config_path())]
        config: String,

        /// Open the configuration file in an editor
        #[arg(short, long)]
        edit: bool,
    },

    /// Migrate configuration to the current schema version
    #[command(before_help = BANNER)]
    MigrateConfig {
        /// Path to the configuration file
        #[arg(short, long, default_value_t = default_config_path())]
        config: String,

        /// Ambiguous migration behavior (fail means non-interactive)
        #[arg(long = "on-ambiguous", value_enum)]
        on_ambiguous: Option<OnAmbiguousOption>,
    },

    /// Set up the clawshell system user and permissions
    #[command(before_help = BANNER)]
    Onboard,

    /// Completely uninstall ClawShell from the system
    #[command(before_help = BANNER)]
    Uninstall {
        /// Skip the confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Show the current version of ClawShell
    #[command(before_help = BANNER)]
    Version,
}
