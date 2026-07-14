use chrono::{DateTime, Utc};

/// One rate-limit window of a provider (e.g. 5h session, 7d weekly).
#[derive(Debug, Clone, PartialEq)]
pub struct RateWindow {
    /// Human label, e.g. "Session", "Weekly", "Weekly (Opus)".
    pub label: String,
    /// Utilization in percent, 0.0..=100.0.
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
    /// Optional detail line under the bar (e.g. dollar spend vs remaining).
    pub caption: Option<String>,
}

impl RateWindow {
    pub fn new(label: impl Into<String>, used_percent: f64) -> Self {
        Self {
            label: label.into(),
            used_percent,
            resets_at: None,
            caption: None,
        }
    }

    pub fn with_caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Credits {
    /// Remaining balance when no breakdown is available.
    pub balance: f64,
    pub currency: Option<String>,
    /// Optional spent amount (e.g. Claude extra usage).
    pub used: Option<f64>,
    /// Optional cap/limit paired with `used`.
    pub limit: Option<f64>,
}

impl Credits {
    pub fn from_balance(balance: f64, currency: Option<String>) -> Self {
        Self {
            balance,
            currency,
            used: None,
            limit: None,
        }
    }
}

/// Result of one successful provider fetch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSnapshot {
    /// Plan name as shown in the badge, e.g. "Pro", "Max", "Plus".
    pub plan: Option<String>,
    /// Account identifier (email) if known.
    pub account: Option<String>,
    /// Windows in display order; first one is the "primary" (session) window.
    pub windows: Vec<RateWindow>,
    pub credits: Option<Credits>,
    pub fetched_at: Option<DateTime<Utc>>,
}

impl UsageSnapshot {
    /// Highest utilization across all windows — drives the tray icon.
    pub fn max_used_percent(&self) -> Option<f64> {
        self.windows
            .iter()
            .map(|w| w.used_percent)
            .fold(None, |acc, p| Some(acc.map_or(p, |a: f64| a.max(p))))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderState {
    Loading,
    Ready(UsageSnapshot),
    Error(String),
}

/// Snapshot of one provider prepared for display (tray + popover).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderDisplay {
    pub id: &'static str,
    pub name: &'static str,
    pub state: ProviderState,
}

/// Events flowing to the GTK main thread.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Full new display state (all enabled providers, display order).
    StateChanged(Vec<ProviderDisplay>),
    /// Show the popover with this provider's view selected; if it is already
    /// visible and showing that provider, hide it (toggle semantics per icon).
    ShowProvider(String),
    /// Toggle the popover (combined tray icon).
    TogglePopover,
    Quit,
}

/// Commands flowing from UI/tray to the fetch engine.
#[derive(Debug, Clone)]
pub enum EngineCommand {
    RefreshAll,
    Quit,
}
