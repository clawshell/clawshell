use console::Style;
use inquire::PasswordDisplayMode;
use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};
use inquire::validator::StringValidator;
use inquire::{Confirm, InquireError, Password, Select, Text};
use std::error::Error;
use std::fmt::Display;
use std::io::{self, Write};

// Theme color RGB values
const THEME_R: u8 = 236;
const THEME_G: u8 = 142;
const THEME_B: u8 = 65;

pub fn theme() -> RenderConfig<'static> {
    let theme_color = Color::rgb(THEME_R, THEME_G, THEME_B);

    RenderConfig {
        prompt_prefix: Styled::new("?").with_style_sheet(
            StyleSheet::new()
                .with_fg(theme_color)
                .with_attr(Attributes::BOLD),
        ),
        answered_prompt_prefix: Styled::new("✓").with_style_sheet(
            StyleSheet::new()
                .with_fg(Color::LightGreen)
                .with_attr(Attributes::BOLD),
        ),
        highlighted_option_prefix: Styled::new("▸").with_style_sheet(
            StyleSheet::new()
                .with_fg(theme_color)
                .with_attr(Attributes::BOLD),
        ),
        prompt: StyleSheet::new().with_attr(Attributes::BOLD),
        answer: StyleSheet::new().with_fg(theme_color),
        default_value: StyleSheet::new().with_fg(Color::DarkGrey),
        help_message: StyleSheet::new().with_fg(Color::DarkGrey),
        ..RenderConfig::default()
    }
}

pub fn theme_style() -> Style {
    Style::new().color256(
        208, // DarkOrange
    )
}

pub fn theme_bold() -> Style {
    Style::new().color256(208).bold()
}

pub fn success_style() -> Style {
    Style::new().green().bold()
}

pub fn error_style() -> Style {
    Style::new().red().bold()
}

pub fn warning_style() -> Style {
    Style::new().yellow().bold()
}

pub fn print_banner(subtitle: &str) {
    let bold = theme_bold();
    let normal = theme_style();

    println!();
    println!(
        "  🦞🦊 {}  {}",
        bold.apply_to("ClawShell"),
        normal.apply_to("Securing OpenClaw"),
    );
    if !subtitle.is_empty() {
        println!("      {}", bold.apply_to(format!("── {subtitle} ──")));
    }
    println!();
}

pub fn print_section(title: &str) {
    let style = theme_bold();
    println!();
    println!("{}", style.apply_to(format!("── {title} ──")));
    println!();
}

/// Print a step indicator like "[2/5] Doing something..." without advancing to the next line,
/// so it can be updated in-place by a subsequent call to [`print_step_done`].
pub fn print_step(step: usize, total: usize, msg: &str) {
    let prefix = theme_style().apply_to(format!("[{step}/{total}]"));
    // Clear the current line and print without newline
    print!("\r\x1b[2K{prefix} {msg}");
    let _ = std::io::stdout().flush();
}

/// Print a step completion message like "[2/5] Done ✓", overwriting the previous step
/// indicator printed by [`print_step`] in-place, and then move to the next line.
pub fn print_step_done(step: usize, total: usize, msg: &str) {
    let prefix = success_style().apply_to(format!("[{step}/{total}]"));
    let check = success_style().apply_to("✓");
    // Clear the current line, print the done message, and move to next line
    println!("\r\x1b[2K{prefix} {msg} {check}");
}

/// Print a noticeable callout box with a title and body lines.
///
/// Layout: each row between `│` and `│` is exactly `inner` display columns.
///   - Title row:   centered `⚠ {title}`
///   - Content row: left-aligned ` {line} `
///
/// Uses `console::measure_text_width` for correct Unicode column widths.
pub fn print_callout(title: &str, lines: &[&str]) {
    let style = warning_style();

    // Build the raw title content (without padding) and measure its display width
    let title_content = format!("⚠ {title}");
    let title_width = console::measure_text_width(&title_content);

    // Each body line is rendered as " {line} " — measure display width
    let body_max = lines
        .iter()
        .map(|l| console::measure_text_width(l) + 2) // " " + line + " "
        .max()
        .unwrap_or(0);

    // inner = display columns between the two │ borders
    let inner = std::cmp::max(title_width + 2, body_max); // +2 for min 1-space padding each side

    let bar = "─".repeat(inner);
    println!();
    println!("{}", style.apply_to(format!("┌{bar}┐")));

    // Center the title
    let total_pad = inner.saturating_sub(title_width);
    let left = total_pad / 2;
    let right = total_pad - left;
    println!(
        "{}",
        style.apply_to(format!(
            "│{}{}{}│",
            " ".repeat(left),
            title_content,
            " ".repeat(right)
        ))
    );

    println!("{}", style.apply_to(format!("├{bar}┤")));

    for line in lines {
        let line_width = console::measure_text_width(line);
        let pad = inner.saturating_sub(line_width + 2);
        println!(
            "{}",
            style.apply_to(format!("│ {line}{} │", " ".repeat(pad)))
        );
    }

    println!("{}", style.apply_to(format!("└{bar}┘")));
    println!();
}

pub fn print_success(msg: &str) {
    let check = success_style().apply_to("✓");
    println!("{check} {msg}");
}

pub fn print_error(msg: &str) {
    let cross = error_style().apply_to("✗");
    eprintln!("{cross} {msg}");
}

pub fn print_warning(msg: &str) {
    let warn = warning_style().apply_to("⚠");
    println!("{warn} {msg}");
}

pub fn print_info(key: &str, value: &str) {
    let key_styled = theme_style().apply_to(format!("{key}:"));
    println!("  {key_styled} {value}");
}

pub fn prompt_text(message: &str, default: Option<&str>) -> Result<String, InquireError> {
    let mut prompt = Text::new(message).with_render_config(theme());

    if let Some(d) = default {
        prompt = prompt.with_default(d);
    }

    prompt.prompt()
}

/// Prompt the user for text input with validation.
pub fn prompt_text_validated<T: StringValidator>(
    message: &str,
    default: Option<&str>,
    validator: T,
) -> Result<String, InquireError> {
    let mut prompt = Text::new(message).with_render_config(theme());

    if let Some(d) = default {
        prompt = prompt.with_default(d);
    }

    prompt.with_validator(validator).prompt()
}

pub fn prompt_password(message: &str) -> Result<String, InquireError> {
    Password::new(message)
        .with_render_config(theme())
        .without_confirmation()
        .with_display_mode(PasswordDisplayMode::Masked)
        .prompt()
}

pub fn prompt_confirm(message: &str, default: bool) -> Result<bool, InquireError> {
    Confirm::new(message)
        .with_render_config(theme())
        .with_default(default)
        .prompt()
}

fn parse_compact_confirm_input(input: &str) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg_attr(test, allow(dead_code))]
pub fn prompt_confirm_compact(message: &str, default: bool) -> Result<bool, Box<dyn Error>> {
    let question_prefix = theme_style().apply_to("?");
    let answer_prefix = success_style().apply_to("✓");
    let warning_prefix = warning_style().apply_to("⚠");
    let suffix = if default { "(Y/n)" } else { "(y/N)" };

    loop {
        print!("{question_prefix} {message} {suffix} ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim();

        let decision = if trimmed.is_empty() {
            Some(default)
        } else {
            parse_compact_confirm_input(trimmed)
        };

        if let Some(approved) = decision {
            let answer = theme_style().apply_to(if approved { "Yes" } else { "No" });
            // Move up to the prompt row, clear it, then print the finalized answer row.
            print!("\x1b[1A\r\x1b[2K{answer_prefix} {message} {answer}\n");
            io::stdout().flush()?;
            return Ok(approved);
        }

        println!("{warning_prefix} Enter y/yes or n/no.");
    }
}

pub fn prompt_select<T: Display>(message: &str, options: Vec<T>) -> Result<T, InquireError> {
    Select::new(message, options)
        .with_render_config(theme())
        .prompt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_returns_render_config() {
        let config = theme();
        // The theme should have our custom prefix
        assert_eq!(config.prompt_prefix.content, "?");
        assert_eq!(config.answered_prompt_prefix.content, "✓");
        assert_eq!(config.highlighted_option_prefix.content, "▸");
    }

    #[test]
    fn test_theme_style_returns_style() {
        let style = theme_style();
        // Just verify it doesn't panic
        let _ = style.apply_to("test");
    }

    #[test]
    fn test_theme_bold_returns_style() {
        let style = theme_bold();
        let _ = style.apply_to("test");
    }

    #[test]
    fn test_success_style() {
        let style = success_style();
        let _ = style.apply_to("ok");
    }

    #[test]
    fn test_error_style() {
        let style = error_style();
        let _ = style.apply_to("fail");
    }

    #[test]
    fn test_warning_style() {
        let style = warning_style();
        let _ = style.apply_to("warn");
    }

    #[test]
    fn test_print_banner_does_not_panic() {
        print_banner("Onboarding");
    }

    #[test]
    fn test_print_banner_empty_subtitle() {
        print_banner("");
    }

    #[test]
    fn test_print_section_does_not_panic() {
        print_section("Test Section");
    }

    #[test]
    fn test_print_step_does_not_panic() {
        print_step(1, 5, "doing something");
    }

    #[test]
    fn test_print_step_done_does_not_panic() {
        print_step_done(1, 5, "did something");
    }

    #[test]
    fn test_print_callout_does_not_panic() {
        print_callout("Test Title", &["Line one", "Line two"]);
    }

    #[test]
    fn test_print_callout_single_line() {
        print_callout("Notice", &["Single line"]);
    }

    #[test]
    fn test_print_success_does_not_panic() {
        print_success("all good");
    }

    #[test]
    fn test_print_error_does_not_panic() {
        print_error("something failed");
    }

    #[test]
    fn test_print_warning_does_not_panic() {
        print_warning("be careful");
    }

    #[test]
    fn test_print_info_does_not_panic() {
        print_info("Key", "Value");
    }

    #[test]
    fn test_parse_compact_confirm_input_yes_variants() {
        assert_eq!(parse_compact_confirm_input("y"), Some(true));
        assert_eq!(parse_compact_confirm_input("Y"), Some(true));
        assert_eq!(parse_compact_confirm_input("yes"), Some(true));
        assert_eq!(parse_compact_confirm_input("YeS"), Some(true));
        assert_eq!(parse_compact_confirm_input(" yes "), Some(true));
    }

    #[test]
    fn test_parse_compact_confirm_input_no_variants() {
        assert_eq!(parse_compact_confirm_input("n"), Some(false));
        assert_eq!(parse_compact_confirm_input("N"), Some(false));
        assert_eq!(parse_compact_confirm_input("no"), Some(false));
        assert_eq!(parse_compact_confirm_input("NO"), Some(false));
        assert_eq!(parse_compact_confirm_input(" no "), Some(false));
    }

    #[test]
    fn test_parse_compact_confirm_input_invalid() {
        assert_eq!(parse_compact_confirm_input(""), None);
        assert_eq!(parse_compact_confirm_input("maybe"), None);
        assert_eq!(parse_compact_confirm_input("1"), None);
    }
}
