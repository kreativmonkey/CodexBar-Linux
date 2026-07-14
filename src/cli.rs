use crate::config::Config;
use crate::model::{Credits, ProviderState, RateWindow, UsageSnapshot};
use crate::providers::{self, Provider};
use crate::reset_time::format_reset_time;
use anyhow::{bail, Context};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "codexbar",
    about = "Fetch AI provider usage from the command line",
    version,
    disable_version_flag = true
)]
pub struct Cli {
    #[arg(short = 'V', long, action = clap::ArgAction::Version)]
    version: (),

    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    usage: UsageArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch provider usage (default when flags are passed)
    Usage(UsageArgs),
}

#[derive(clap::Args, Default, Clone)]
pub struct UsageArgs {
    /// Provider id (`claude`, `codex`, …) or `all`
    #[arg(long)]
    provider: Option<String>,

    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    #[arg(long, default_value_t = false)]
    pretty: bool,

    #[arg(long)]
    no_color: bool,
}

#[derive(Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug)]
pub struct ProviderResult {
    pub id: &'static str,
    pub name: &'static str,
    pub state: ProviderState,
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let args = match cli.command {
        Some(Command::Usage(args)) => args,
        None => cli.usage,
    };
    let results = fetch_selected(&args)?;
    let any_error = results
        .iter()
        .any(|r| matches!(r.state, ProviderState::Error(_)));
    match args.format {
        OutputFormat::Text => print_text(&results, use_color(&args))?,
        OutputFormat::Json => print_json(&results, args.pretty)?,
    }
    if any_error {
        std::process::exit(1);
    }
    Ok(())
}

fn use_color(args: &UsageArgs) -> bool {
    if args.no_color || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    io::stdout().is_terminal()
}

pub fn resolve_args(args: &UsageArgs) -> anyhow::Result<Vec<Arc<dyn Provider>>> {
    let cfg = Config::load();
    crate::config::init_global(cfg.clone());
    let (providers, auto_detect) = providers::enabled_providers(&cfg.providers);
    select_providers(providers, args.provider.as_deref(), auto_detect)
}

async fn fetch_selected_async(args: &UsageArgs) -> anyhow::Result<Vec<ProviderResult>> {
    let selected = resolve_args(args)?;
    if selected.is_empty() {
        bail!("no providers selected — configure credentials or pass --provider <id>");
    }

    let mut handles = Vec::with_capacity(selected.len());
    for provider in selected {
        handles.push(tokio::spawn(async move {
            let state = match provider.fetch().await {
                Ok(snapshot) => ProviderState::Ready(snapshot),
                Err(err) => ProviderState::Error(format!("{err:#}")),
            };
            ProviderResult {
                id: provider.id(),
                name: provider.display_name(),
                state,
            }
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await.context("provider fetch task failed")?);
    }
    Ok(results)
}

pub fn fetch_selected(args: &UsageArgs) -> anyhow::Result<Vec<ProviderResult>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start tokio runtime")?;
    rt.block_on(fetch_selected_async(args))
}

fn select_providers(
    providers: Vec<Arc<dyn Provider>>,
    filter: Option<&str>,
    auto_detect: bool,
) -> anyhow::Result<Vec<Arc<dyn Provider>>> {
    match filter {
        None => Ok(providers
            .into_iter()
            .filter(|p| !auto_detect || p.is_configured())
            .collect()),
        Some("all") => Ok(providers),
        Some(id) => {
            let picked: Vec<_> = providers.into_iter().filter(|p| p.id() == id).collect();
            if picked.is_empty() {
                bail!("unknown provider '{id}' — run with --help for supported ids");
            }
            Ok(picked)
        }
    }
}

fn print_text(results: &[ProviderResult], color: bool) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for (idx, result) in results.iter().enumerate() {
        if idx > 0 {
            writeln!(out)?;
        }
        write_provider_text(&mut out, result, color)?;
    }
    Ok(())
}

fn write_provider_text(
    out: &mut impl Write,
    result: &ProviderResult,
    color: bool,
) -> io::Result<()> {
    let header = if color {
        format!("\x1b[1m== {} ==\x1b[0m", result.name)
    } else {
        format!("== {} ==", result.name)
    };
    writeln!(out, "{header}")?;

    match &result.state {
        ProviderState::Loading => writeln!(out, "Loading…")?,
        ProviderState::Error(msg) => {
            if color {
                writeln!(out, "\x1b[31mError:\x1b[0m {msg}")?;
            } else {
                writeln!(out, "Error: {msg}")?;
            }
        }
        ProviderState::Ready(snap) => write_snapshot_text(out, snap, color)?,
    }
    Ok(())
}

fn write_snapshot_text(out: &mut impl Write, snap: &UsageSnapshot, color: bool) -> io::Result<()> {
    if let Some(plan) = &snap.plan {
        writeln!(out, "Plan: {plan}")?;
    }
    if let Some(account) = &snap.account {
        writeln!(out, "Account: {account}")?;
    }
    for window in &snap.windows {
        write_window_line(out, window, color)?;
    }
    if let Some(credits) = &snap.credits {
        writeln!(out, "{}", format_credits_line(credits))?;
    }
    Ok(())
}

fn write_window_line(out: &mut impl Write, window: &RateWindow, color: bool) -> io::Result<()> {
    let pct = window.used_percent.clamp(0.0, 100.0);
    let left = (100.0 - pct).clamp(0.0, 100.0);
    let bar = usage_bar(pct, 12);
    let line = if color {
        let color_code = severity_color(pct);
        format!(
            "{label}: {left:.0}% left [{bar}] \x1b[{color_code}m{pct:.0}% used\x1b[0m",
            label = window.label,
        )
    } else {
        format!(
            "{label}: {left:.0}% left [{bar}] {pct:.0}% used",
            label = window.label,
        )
    };
    writeln!(out, "{line}")?;
    if let Some(caption) = &window.caption {
        writeln!(out, "  {caption}")?;
    }
    if let Some(resets_at) = window.resets_at {
        writeln!(out, "  {}", format_reset_time(resets_at))?;
    }
    Ok(())
}

fn severity_color(used_percent: f64) -> u8 {
    if used_percent >= 90.0 {
        31
    } else if used_percent >= 70.0 {
        33
    } else {
        32
    }
}

fn usage_bar(used_percent: f64, width: usize) -> String {
    let filled = ((used_percent / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

fn format_credits_line(credits: &Credits) -> String {
    let symbol = credits.currency.as_deref().unwrap_or("$");
    match (credits.used, credits.limit) {
        (Some(used), Some(limit)) => format!("Extra usage: {symbol}{used:.2} / {symbol}{limit:.2}"),
        _ => format!("Credits: {symbol}{:.2}", credits.balance),
    }
}

#[derive(Serialize)]
struct JsonOutput<'a> {
    providers: Vec<JsonProvider<'a>>,
}

#[derive(Serialize)]
struct JsonProvider<'a> {
    id: &'a str,
    name: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    windows: Vec<JsonWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credits: Option<JsonCredits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fetched_at: Option<String>,
}

#[derive(Serialize)]
struct JsonWindow {
    label: String,
    used_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caption: Option<String>,
}

#[derive(Serialize)]
struct JsonCredits {
    balance: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<f64>,
}

fn print_json(results: &[ProviderResult], pretty: bool) -> io::Result<()> {
    let payload = JsonOutput {
        providers: results.iter().map(json_provider).collect(),
    };
    let mut out = io::stdout().lock();
    if pretty {
        serde_json::to_writer_pretty(&mut out, &payload)?;
    } else {
        serde_json::to_writer(&mut out, &payload)?;
    }
    writeln!(out)?;
    Ok(())
}

fn json_provider(result: &ProviderResult) -> JsonProvider<'_> {
    match &result.state {
        ProviderState::Loading => JsonProvider {
            id: result.id,
            name: result.name,
            ok: false,
            error: Some("loading"),
            plan: None,
            account: None,
            windows: Vec::new(),
            credits: None,
            fetched_at: None,
        },
        ProviderState::Error(msg) => JsonProvider {
            id: result.id,
            name: result.name,
            ok: false,
            error: Some(msg.as_str()),
            plan: None,
            account: None,
            windows: Vec::new(),
            credits: None,
            fetched_at: None,
        },
        ProviderState::Ready(snap) => JsonProvider {
            id: result.id,
            name: result.name,
            ok: true,
            error: None,
            plan: snap.plan.as_deref(),
            account: snap.account.as_deref(),
            windows: snap
                .windows
                .iter()
                .map(|w| JsonWindow {
                    label: w.label.clone(),
                    used_percent: w.used_percent,
                    resets_at: w.resets_at.map(|t| t.to_rfc3339()),
                    caption: w.caption.clone(),
                })
                .collect(),
            credits: snap.credits.as_ref().map(|c| JsonCredits {
                balance: c.balance,
                currency: c.currency.clone(),
                used: c.used,
                limit: c.limit,
            }),
            fetched_at: snap.fetched_at.map(|t| t.to_rfc3339()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Credits, RateWindow, UsageSnapshot};
    use chrono::Utc;

    fn sample_result() -> ProviderResult {
        ProviderResult {
            id: "claude",
            name: "Claude",
            state: ProviderState::Ready(UsageSnapshot {
                plan: Some("Pro".to_string()),
                account: Some("user@example.com".to_string()),
                windows: vec![RateWindow {
                    label: "Session".to_string(),
                    used_percent: 42.0,
                    resets_at: None,
                    caption: None,
                }],
                credits: Some(Credits {
                    balance: 74.5,
                    currency: Some("USD".to_string()),
                    used: Some(25.5),
                    limit: Some(100.0),
                }),
                fetched_at: Some(Utc::now()),
            }),
        }
    }

    #[test]
    fn text_output_includes_provider_header() {
        let mut buf = Vec::new();
        write_provider_text(&mut buf, &sample_result(), false).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("== Claude =="));
        assert!(text.contains("Plan: Pro"));
        assert!(text.contains("Session:"));
        assert!(text.contains("Extra usage: USD25.50 / USD100.00"));
    }

    #[test]
    fn json_output_marks_success() {
        let result = sample_result();
        let payload = JsonOutput {
            providers: vec![json_provider(&result)],
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"id\":\"claude\""));
    }

    #[test]
    fn select_provider_by_id() {
        let all = providers::all_providers();
        let picked = select_providers(all, Some("codex"), true).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].id(), "codex");
    }

    #[test]
    fn select_unknown_provider_errors() {
        let all = providers::all_providers();
        assert!(select_providers(all, Some("nope"), true).is_err());
    }

    #[test]
    fn usage_bar_fills_by_percent() {
        assert_eq!(usage_bar(50.0, 10), "[█████░░░░░]");
        assert_eq!(usage_bar(100.0, 4), "[████]");
    }
}
