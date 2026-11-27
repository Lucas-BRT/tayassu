use crate::render::{format_countdown, render_login};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{self, Stdout};
use std::time::Duration;
use tayassu_core::{AccountConfig, AccountId, AccountStatus, ModelBucket};
use tayassu_store::RuntimeState;

/// Poll interval in milliseconds for live cooldown countdown ticks in the TUI.
const EVENT_POLL_MS: u64 = 500;

/// Header pane height in terminal rows.
const HEADER_HEIGHT: u16 = 3;

/// Minimum height of the details pane, in terminal rows (borders included).
const DETAIL_MIN_HEIGHT: u16 = 3;

/// Maximum height of the details pane before its content scrolls out of view.
const DETAIL_MAX_HEIGHT: u16 = 12;

/// Footer help bar height in terminal rows.
const FOOTER_HEIGHT: u16 = 1;

/// Column widths in the main accounts table.
const COL_ACCOUNT_WIDTH: u16 = 18;
const COL_PROVIDER_WIDTH: u16 = 10;
const COL_PRIORITY_WIDTH: u16 = 5;
const COL_STATE_WIDTH: u16 = 18;
/// Minimum quota column width, used on narrow terminals.
const COL_QUOTA_MIN_WIDTH: u16 = 12;
/// Minimum label column width; zero lets it collapse before the quota column clips.
const COL_LABEL_MIN_WIDTH: u16 = 0;
/// Horizontal padding added to the widest quota cell.
const COL_CELL_PADDING: u16 = 2;
/// Space ratatui inserts between table columns.
const COLUMN_SPACING: u16 = 1;
/// Table borders (2) plus the selection symbol (2).
const TABLE_CHROME_WIDTH: u16 = 4;
/// Header text for the quota column.
const COL_QUOTA_HEADER: &str = "QUOTA";

/// Visual state of an account row in the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountState {
    /// Account is disabled in `config.toml`.
    Disabled,
    /// Last probe failed because the profile needs a fresh login.
    AuthRequired,
    /// Last probe failed for another reason.
    ProbeError,
    /// Account has an active cooldown on the evaluated bucket.
    Cooling(Option<chrono::TimeDelta>),
    /// Account has no remaining quota on the evaluated bucket.
    Exhausted,
    /// Account is available on the default bucket but has a sub-bucket cooldown.
    Partial,
    /// No current quota measurement is available.
    Unknown,
    /// Account is fully available.
    Ready,
}

impl AccountState {
    /// Sort severity, with the most actionable states first.
    const fn severity(self) -> u8 {
        match self {
            Self::AuthRequired => 0,
            Self::ProbeError => 1,
            Self::Cooling(_) => 2,
            Self::Exhausted => 3,
            Self::Partial => 4,
            Self::Disabled => 5,
            Self::Unknown => 6,
            Self::Ready => 7,
        }
    }
}

/// Ordering applied to the accounts table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    /// Original order from `config.toml`.
    Configured,
    /// Lowest priority value first.
    Priority,
    /// Provider name, then priority.
    Provider,
    /// Most actionable state first.
    State,
}

impl SortMode {
    /// Advances to the next sort mode.
    const fn next(self) -> Self {
        match self {
            Self::Configured => Self::Priority,
            Self::Priority => Self::Provider,
            Self::Provider => Self::State,
            Self::State => Self::Configured,
        }
    }

    /// Short label shown in the table title.
    const fn label(self) -> &'static str {
        match self {
            Self::Configured => "config",
            Self::Priority => "priority",
            Self::Provider => "provider",
            Self::State => "state",
        }
    }
}

/// Row filter applied to the accounts table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterMode {
    /// Show every account.
    All,
    /// Show only accounts that need attention.
    Problems,
    /// Show only fully ready accounts.
    Ready,
}

impl FilterMode {
    /// Advances to the next filter.
    const fn next(self) -> Self {
        match self {
            Self::All => Self::Problems,
            Self::Problems => Self::Ready,
            Self::Ready => Self::All,
        }
    }

    /// Short label shown in the table title.
    const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Problems => "problems",
            Self::Ready => "ready",
        }
    }

    /// Returns `true` when `state` should be visible.
    const fn matches(self, state: AccountState) -> bool {
        match self {
            Self::All => true,
            Self::Problems => !matches!(state, AccountState::Ready | AccountState::Unknown),
            Self::Ready => matches!(state, AccountState::Ready),
        }
    }
}

/// Actions returned by keyboard input handling in [`DashboardState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashAction {
    /// No external side effect required.
    None,
    /// Probe all configured accounts and refresh runtime state.
    ProbeAll,
    /// Clear active cooldowns on the currently selected account.
    ClearSelectedCooldowns,
    /// Exit the dashboard cleanly.
    Quit,
}

/// Interactive state and pure renderer for `tayassu dash`.
#[derive(Debug, Clone)]
pub struct DashboardState {
    selected: usize,
    status_message: String,
    sort_mode: SortMode,
    filter_mode: FilterMode,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            selected: 0,
            status_message: "Ready".to_owned(),
            sort_mode: SortMode::Configured,
            filter_mode: FilterMode::All,
        }
    }
}

impl DashboardState {
    /// Updates the transient status message shown in the footer.
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = message.into();
    }

    /// Returns the accounts visible under the current filter, in sort order.
    #[must_use]
    pub fn visible_accounts<'a>(
        &self,
        accounts: &'a [AccountConfig],
        statuses: &[AccountStatus],
        now: DateTime<Utc>,
    ) -> Vec<&'a AccountConfig> {
        let default_bucket = ModelBucket::default_bucket();
        let mut visible: Vec<&AccountConfig> = accounts
            .iter()
            .filter(|account| {
                let status = statuses.iter().find(|st| st.account_id == account.id);
                self.filter_mode
                    .matches(account_state(account, status, &default_bucket, now))
            })
            .collect();

        match self.sort_mode {
            SortMode::Configured => {}
            SortMode::Priority => {
                visible.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
            }
            SortMode::Provider => visible.sort_by(|a, b| {
                a.provider
                    .as_str()
                    .cmp(b.provider.as_str())
                    .then_with(|| a.priority.cmp(&b.priority))
                    .then_with(|| a.id.cmp(&b.id))
            }),
            SortMode::State => visible.sort_by(|a, b| {
                let a_state = account_state(
                    a,
                    statuses.iter().find(|st| st.account_id == a.id),
                    &default_bucket,
                    now,
                );
                let b_state = account_state(
                    b,
                    statuses.iter().find(|st| st.account_id == b.id),
                    &default_bucket,
                    now,
                );
                a_state
                    .severity()
                    .cmp(&b_state.severity())
                    .then_with(|| a.priority.cmp(&b.priority))
                    .then_with(|| a.id.cmp(&b.id))
            }),
        }

        visible
    }

    /// Returns the selected account from a precomputed visible list.
    #[must_use]
    pub fn selected_visible_account<'a>(
        &self,
        visible: &[&'a AccountConfig],
    ) -> Option<&'a AccountConfig> {
        visible.get(self.selected).copied()
    }

    /// Advances selection to the next account (wrapping around).
    pub fn select_next(&mut self, count: usize) {
        if count > 0 {
            self.selected = (self.selected + 1) % count;
        }
    }

    /// Moves selection to the previous account (wrapping around).
    pub fn select_prev(&mut self, count: usize) {
        if count > 0 {
            self.selected = if self.selected == 0 {
                count - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Clamps selection index when the account list shrinks.
    pub fn clamp_selection(&mut self, count: usize) {
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    /// Translates a key event into a [`DashAction`].
    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        account_count: usize,
    ) -> DashAction {
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return DashAction::Quit;
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => DashAction::Quit,
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next(account_count);
                DashAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev(account_count);
                DashAction::None
            }
            KeyCode::Char('r') => DashAction::ProbeAll,
            KeyCode::Char('c') => DashAction::ClearSelectedCooldowns,
            KeyCode::Char('s') => {
                self.sort_mode = self.sort_mode.next();
                self.selected = 0;
                DashAction::None
            }
            KeyCode::Char('f') => {
                self.filter_mode = self.filter_mode.next();
                self.selected = 0;
                DashAction::None
            }
            _ => DashAction::None,
        }
    }

    /// Renders the full dashboard into `frame` at instant `now`.
    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        accounts: &[AccountConfig],
        statuses: &[AccountStatus],
        now: DateTime<Utc>,
    ) {
        let visible = self.visible_accounts(accounts, statuses, now);
        let detail_lines = visible.get(self.selected).map_or_else(
            || {
                vec![Line::from(if accounts.is_empty() {
                    "No accounts configured. Run: tayassu account add <id> <provider>"
                } else {
                    "No accounts match the current filter."
                })]
            },
            |account| {
                let status = statuses.iter().find(|entry| entry.account_id == account.id);
                build_detail_lines(account, status, now)
            },
        );
        let detail_height = u16::try_from(detail_lines.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .clamp(DETAIL_MIN_HEIGHT, DETAIL_MAX_HEIGHT)
            .min(
                frame
                    .area()
                    .height
                    .saturating_sub(HEADER_HEIGHT + FOOTER_HEIGHT + 3),
            );

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(HEADER_HEIGHT),
                Constraint::Min(0),
                Constraint::Length(detail_height),
                Constraint::Length(FOOTER_HEIGHT),
            ])
            .split(frame.area());

        render_header(frame, chunks[0], accounts, statuses, now);
        self.render_table(frame, chunks[1], &visible, statuses, now);
        Self::render_details(frame, chunks[2], detail_lines);
        self.render_footer(frame, chunks[3]);
    }

    fn render_table(
        &self,
        frame: &mut Frame<'_>,
        area: ratatui::layout::Rect,
        accounts: &[&AccountConfig],
        statuses: &[AccountStatus],
        now: DateTime<Utc>,
    ) {
        let default_bucket = ModelBucket::default_bucket();
        let header = Row::new(["ACCOUNT", "PROVIDER", "PRI", "STATE", "QUOTA", "LABEL"])
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        let rows: Vec<Row<'_>> = accounts
            .iter()
            .map(|account| {
                let status = statuses.iter().find(|entry| entry.account_id == account.id);
                let (state_text, state_color) =
                    account_state_cell(account, status, &default_bucket, now);
                let quota_lines = summarize_buckets(status, now);
                let quota_height = u16::try_from(quota_lines.len()).unwrap_or(u16::MAX);
                let quota_cell = Cell::from(Text::from(
                    quota_lines.into_iter().map(Line::from).collect::<Vec<_>>(),
                ));

                Row::new([
                    Cell::from(account.id.as_str().to_owned()),
                    Cell::from(account.provider.as_str().to_owned()),
                    Cell::from(account.priority.to_string()),
                    Cell::from(state_text).style(Style::default().fg(state_color)),
                    quota_cell,
                    Cell::from(account.label.clone()),
                ])
                .height(quota_height)
            })
            .collect();

        let quota_width = quota_column_width(accounts, statuses, now, area.width);
        let widths = [
            Constraint::Length(COL_ACCOUNT_WIDTH),
            Constraint::Length(COL_PROVIDER_WIDTH),
            Constraint::Length(COL_PRIORITY_WIDTH),
            Constraint::Length(COL_STATE_WIDTH),
            Constraint::Length(quota_width),
            Constraint::Min(COL_LABEL_MIN_WIDTH),
        ];

        let title = format!(
            " Accounts [sort: {} | filter: {}] ",
            self.sort_mode.label(),
            self.filter_mode.label()
        );
        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().title(title).borders(Borders::ALL))
            .row_highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");

        let mut table_state = TableState::default();
        if !accounts.is_empty() {
            table_state.select(Some(self.selected.min(accounts.len() - 1)));
        }
        frame.render_stateful_widget(table, area, &mut table_state);
    }

    fn render_details(frame: &mut Frame<'_>, area: ratatui::layout::Rect, lines: Vec<Line<'_>>) {
        let details = Paragraph::new(lines)
            .block(Block::default().title(" Details ").borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        frame.render_widget(details, area);
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: ratatui::layout::Rect) {
        let footer = Paragraph::new(Line::from(vec![
            Span::styled(
                " j/k: move | s: sort | f: filter | r: probe | c: clear | q: quit ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                self.status_message.as_str(),
                Style::default().fg(Color::Gray),
            ),
        ]));
        frame.render_widget(footer, area);
    }
}

/// RAII guard restoring raw mode and the primary screen buffer on drop.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        match Self::enter_alternate_screen() {
            Ok(guard) => Ok(guard),
            Err(err) => {
                let _ = disable_raw_mode();
                Err(err)
            }
        }
    }

    fn enter_alternate_screen() -> Result<Self> {
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("failed to create ratatui terminal")?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// Runs the interactive `tayassu dash` event loop.
///
/// # Errors
///
/// Returns an error if terminal initialization, drawing, or event polling fails.
pub fn run_dashboard<FProbe, FClear>(
    accounts: &[AccountConfig],
    runtime_state: &mut RuntimeState,
    mut on_probe_all: FProbe,
    mut on_clear_cooldowns: FClear,
) -> Result<()>
where
    FProbe: FnMut(&mut RuntimeState) -> Result<String>,
    FClear: FnMut(&mut RuntimeState, &AccountId) -> Result<String>,
{
    let mut guard = TerminalGuard::enter()?;
    let mut ui = DashboardState::default();

    loop {
        let now = Utc::now();
        let visible = ui.visible_accounts(accounts, &runtime_state.statuses, now);
        ui.clamp_selection(visible.len());

        guard
            .terminal
            .draw(|frame| ui.render(frame, accounts, &runtime_state.statuses, now))
            .context("failed to draw TUI frame")?;

        if event::poll(Duration::from_millis(EVENT_POLL_MS)).context("failed to poll key events")?
            && let Event::Key(key) = event::read().context("failed to read key event")?
            && key.kind == KeyEventKind::Press
        {
            match ui.handle_key(key.code, key.modifiers, visible.len()) {
                DashAction::None => {}
                DashAction::Quit => break,
                DashAction::ProbeAll => {
                    ui.set_status("Probing all accounts...");
                    let _ = guard.terminal.draw(|frame| {
                        ui.render(frame, accounts, &runtime_state.statuses, Utc::now());
                    });
                    match on_probe_all(runtime_state) {
                        Ok(summary) => ui.set_status(summary),
                        Err(err) => ui.set_status(format!("Probe error: {err}")),
                    }
                }
                DashAction::ClearSelectedCooldowns => {
                    if let Some(account) = ui.selected_visible_account(&visible) {
                        match on_clear_cooldowns(runtime_state, &account.id) {
                            Ok(summary) => ui.set_status(summary),
                            Err(err) => ui.set_status(format!("Clear error: {err}")),
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn render_header(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    accounts: &[AccountConfig],
    statuses: &[AccountStatus],
    now: DateTime<Utc>,
) {
    let default_bucket = ModelBucket::default_bucket();
    let total = accounts.len();
    let mut unknown = 0u16;
    let mut ready = 0u16;
    let mut partial = 0u16;
    let mut unavailable = 0u16;
    let mut failed = 0u16;
    let mut disabled = 0u16;

    for account in accounts {
        let status = statuses.iter().find(|st| st.account_id == account.id);
        match account_state(account, status, &default_bucket, now) {
            AccountState::Ready => ready += 1,
            AccountState::Unknown => unknown += 1,
            AccountState::Partial => partial += 1,
            AccountState::Cooling(_) | AccountState::Exhausted => unavailable += 1,
            AccountState::AuthRequired | AccountState::ProbeError => failed += 1,
            AccountState::Disabled => disabled += 1,
        }
    }

    let mut summary = format!(
        "| {total} accounts  •  {ready} ready  •  {unknown} unknown  •  {partial} partial  •  {unavailable} unavailable  •  {failed} failed"
    );
    if disabled > 0 {
        let _ = write!(summary, "  •  {disabled} disabled");
    }
    let _ = write!(summary, "  •  {}", now.format("%H:%M:%S UTC"));

    let line = Line::from(vec![
        Span::styled(
            " tayassu ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(summary),
    ]);

    let header = Paragraph::new(line).block(
        Block::default()
            .title(" AI CLI Session & Cooldown Dashboard ")
            .borders(Borders::ALL),
    );
    frame.render_widget(header, area);
}

fn account_state(
    account: &AccountConfig,
    status: Option<&AccountStatus>,
    bucket: &ModelBucket,
    now: DateTime<Utc>,
) -> AccountState {
    if !account.enabled {
        return AccountState::Disabled;
    }
    if let Some(failure) = status.and_then(|st| st.last_probe_error.as_ref()) {
        return if failure.is_authentication() {
            AccountState::AuthRequired
        } else {
            AccountState::ProbeError
        };
    }
    if let Some(cd) = status.and_then(|st| st.active_cooldown(bucket, now)) {
        return AccountState::Cooling(cd.remaining(now));
    }
    if status.is_some_and(|st| !st.is_available(bucket, now)) {
        return AccountState::Exhausted;
    }
    let has_sub_cooldown = status.is_some_and(|st| {
        st.cooldowns.iter().any(|cd| cd.is_active(now))
            || st.windows.iter().any(|window| window.is_exhausted_at(now))
    });
    if has_sub_cooldown {
        return AccountState::Partial;
    }
    if status.is_none_or(|st| st.windows.iter().all(|window| window.remaining.is_none())) {
        return AccountState::Unknown;
    }
    AccountState::Ready
}

fn account_state_cell(
    account: &AccountConfig,
    status: Option<&AccountStatus>,
    bucket: &ModelBucket,
    now: DateTime<Utc>,
) -> (String, Color) {
    match account_state(account, status, bucket, now) {
        AccountState::Disabled => ("DISABLED".to_owned(), Color::DarkGray),
        AccountState::AuthRequired => ("AUTH".to_owned(), Color::Yellow),
        AccountState::ProbeError => ("ERROR".to_owned(), Color::Red),
        AccountState::Cooling(remaining) => (
            format!(
                "COOLDOWN ({})",
                remaining.map_or_else(|| "0m".to_owned(), format_countdown)
            ),
            Color::Red,
        ),
        AccountState::Exhausted => ("EXHAUSTED".to_owned(), Color::Red),
        AccountState::Partial => ("PARTIAL".to_owned(), Color::Yellow),
        AccountState::Unknown => ("UNKNOWN".to_owned(), Color::Gray),
        AccountState::Ready => ("READY".to_owned(), Color::Green),
    }
}

fn quota_column_width(
    accounts: &[&AccountConfig],
    statuses: &[AccountStatus],
    now: DateTime<Utc>,
    table_width: u16,
) -> u16 {
    let content_width = accounts
        .iter()
        .map(|account| {
            let status = statuses.iter().find(|st| st.account_id == account.id);
            summarize_buckets(status, now)
                .iter()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0);
    let desired = u16::try_from(content_width)
        .unwrap_or(u16::MAX)
        .max(u16::try_from(COL_QUOTA_HEADER.len()).unwrap_or(0))
        + COL_CELL_PADDING;
    let fixed = COL_ACCOUNT_WIDTH
        + COL_PROVIDER_WIDTH
        + COL_PRIORITY_WIDTH
        + COL_STATE_WIDTH
        + COL_LABEL_MIN_WIDTH
        + COLUMN_SPACING * 5
        + TABLE_CHROME_WIDTH;
    let available = table_width.saturating_sub(fixed);
    desired.min(available.max(COL_QUOTA_MIN_WIDTH))
}

fn summarize_buckets(status: Option<&AccountStatus>, now: DateTime<Utc>) -> Vec<String> {
    let Some(st) = status else {
        return vec!["unknown".to_owned()];
    };
    let mut buckets = BTreeSet::new();
    for win in st
        .windows
        .iter()
        .filter(|window| window.remaining.is_some())
    {
        buckets.insert(win.bucket.clone());
    }
    for cd in &st.cooldowns {
        if cd.is_active(now) {
            buckets.insert(cd.bucket.clone());
        }
    }
    if buckets.is_empty() {
        return vec!["unknown".to_owned()];
    }
    buckets
        .iter()
        .map(|bucket| {
            let pct = st.lowest_remaining_percent(bucket, now).map_or_else(
                || "unknown".to_owned(),
                |val| {
                    let stale = if st.last_probe_error.is_some() {
                        " stale"
                    } else {
                        ""
                    };
                    format!("{val}{stale}")
                },
            );
            if let Some(cd) = st.active_cooldown(bucket, now) {
                let rem = cd
                    .remaining(now)
                    .map_or_else(|| "0m".to_owned(), format_countdown);
                format!("{bucket}:{pct}({rem})")
            } else {
                format!("{bucket}:{pct}")
            }
        })
        .collect()
}

fn build_detail_lines<'a>(
    account: &'a AccountConfig,
    status: Option<&'a AccountStatus>,
    now: DateTime<Utc>,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let probed = status.and_then(|st| st.last_probed_at).map_or_else(
        || "never".to_owned(),
        |ts| ts.format("%Y-%m-%d %H:%M UTC").to_string(),
    );

    lines.push(Line::from(format!(
        "Account: {} ({})  |  Executable: {}  |  Login: {}  |  Last probe: {}",
        account.id,
        account.provider,
        account.executable(),
        render_login(status),
        probed
    )));

    if let Some(failure) = status.and_then(|st| st.last_probe_error.as_ref()) {
        let color = if failure.is_authentication() {
            Color::Yellow
        } else {
            Color::Red
        };
        lines.push(Line::from(vec![
            Span::styled("Probe error: ", Style::default().fg(color)),
            Span::raw(failure.message.clone()),
        ]));
    }

    if let Some(st) = status {
        if !st.windows.is_empty() {
            let windows_summary: Vec<String> = st
                .windows
                .iter()
                .map(|win| {
                    let pct = win.remaining.map_or_else(
                        || "unknown quota".to_owned(),
                        |val| {
                            if win.resets_at.is_some_and(|reset| reset <= now) {
                                format!("{val} (expired)")
                            } else {
                                val.to_string()
                            }
                        },
                    );
                    format!("{}/{}={}", win.bucket, win.window_name, pct)
                })
                .collect();
            lines.push(Line::from(format!(
                "Windows{}: {}",
                if st.last_probe_error.is_some() {
                    " (last successful probe)"
                } else {
                    ""
                },
                windows_summary.join("  ")
            )));
        }

        if let Some(budget) = &st.key_budget {
            let stale = if st.last_probe_error.is_some() {
                " (stale)"
            } else {
                ""
            };
            lines.push(Line::from(format!(
                "OpenRouter budget: ${} remaining{stale}",
                budget.remaining_usd
            )));
        }

        let active_cds: Vec<String> = st
            .cooldowns
            .iter()
            .filter(|cd| cd.is_active(now))
            .map(|cd| {
                let rem = cd
                    .remaining(now)
                    .map_or_else(|| "0m".to_owned(), format_countdown);
                format!("{} ({rem}, {})", cd.bucket, cd.reason)
            })
            .collect();
        if !active_cds.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Cooldowns: ", Style::default().fg(Color::Red)),
                Span::raw(active_cds.join("  ")),
            ]));
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use ratatui::backend::TestBackend;
    use tayassu_core::{CooldownEntry, ProbeFailure, ProviderKind, QuotaWindow, RemainingPercent};

    /// Virtual terminal width for `TestBackend` rendering checks.
    const TEST_TERM_WIDTH: u16 = 100;

    /// Virtual terminal height for `TestBackend` rendering checks.
    const TEST_TERM_HEIGHT: u16 = 20;

    /// Wider terminal used to verify quota cells are not clipped.
    const WIDE_TEST_TERM_WIDTH: u16 = 120;

    /// Narrow terminal used to verify quota cells survive reduced layouts.
    const NARROW_TEST_TERM_WIDTH: u16 = 80;

    /// Gemini remaining quota fixture percentage.
    const TEST_GEMINI_PCT: u8 = 31;

    /// Claude remaining quota fixture percentage.
    const TEST_CLAUDE_PCT: u8 = 50;

    /// Cooldown duration in minutes used in TUI rendering tests.
    const TEST_COOLDOWN_MINUTES: i64 = 90;

    /// Remaining quota fixture percentage.
    const TEST_REMAINING_PCT: u8 = 47;

    #[test]
    fn dashboard_renders_accounts_and_cooldowns_into_test_backend() {
        let now = Utc::now();
        let account = AccountConfig::new(
            "agy-student".parse().expect("id"),
            ProviderKind::Agy,
            "Student Plan",
        );
        let mut status = AccountStatus::empty(account.id.clone());
        status.windows.push(QuotaWindow {
            window_name: "5h".to_owned(),
            bucket: "gemini".parse().expect("bucket"),
            remaining: Some(RemainingPercent::try_from(TEST_REMAINING_PCT).expect("pct")),
            resets_at: None,
        });
        status.set_cooldown(CooldownEntry {
            bucket: "claude".parse().expect("bucket"),
            resets_at: now + TimeDelta::minutes(TEST_COOLDOWN_MINUTES),
            reason: "agy-probe:5h".to_owned(),
        });

        let state = DashboardState::default();
        let backend = TestBackend::new(TEST_TERM_WIDTH, TEST_TERM_HEIGHT);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let accounts = [account];
        let statuses = [status];

        terminal
            .draw(|frame| state.render(frame, &accounts, &statuses, now))
            .expect("draw frame");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(rendered.contains("agy-student"));
        assert!(rendered.contains("PARTIAL"));
        assert!(rendered.contains("gemini:47%"));
        assert!(rendered.contains("claude:unknown(1h30m)"));
    }

    #[test]
    fn dashboard_keeps_full_quota_text_and_counts_partial_accounts() {
        let now = Utc::now();
        let account =
            AccountConfig::new("agy-host".parse().expect("id"), ProviderKind::Agy, "Host");
        let mut status = AccountStatus::empty(account.id.clone());

        for (bucket, pct) in [("gemini", TEST_GEMINI_PCT), ("claude", TEST_CLAUDE_PCT)] {
            let bucket: ModelBucket = bucket.parse().expect("bucket");
            status.windows.push(QuotaWindow {
                window_name: "weekly".to_owned(),
                bucket: bucket.clone(),
                remaining: Some(RemainingPercent::try_from(pct).expect("pct")),
                resets_at: None,
            });
            status.set_cooldown(CooldownEntry {
                bucket,
                resets_at: now + TimeDelta::minutes(TEST_COOLDOWN_MINUTES),
                reason: "agy-probe:5h".to_owned(),
            });
        }

        let state = DashboardState::default();
        let backend = TestBackend::new(WIDE_TEST_TERM_WIDTH, TEST_TERM_HEIGHT);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let accounts = [account];
        let statuses = [status];

        terminal
            .draw(|frame| state.render(frame, &accounts, &statuses, now))
            .expect("draw frame");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(rendered.contains("claude:50%(1h30m)"), "{rendered}");
        assert!(rendered.contains("gemini:31%(1h30m)"), "{rendered}");
        assert!(rendered.contains("0 ready"));
        assert!(rendered.contains("1 partial"));
        assert!(rendered.contains("0 unavailable"));
        assert!(!rendered.contains("Press 'r' to probe accounts"));

        let backend = TestBackend::new(NARROW_TEST_TERM_WIDTH, TEST_TERM_HEIGHT);
        let mut terminal = Terminal::new(backend).expect("narrow test terminal");
        terminal
            .draw(|frame| state.render(frame, &accounts, &statuses, now))
            .expect("draw narrow frame");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(rendered.contains("claude:50%(1h30m)"), "{rendered}");
        assert!(rendered.contains("gemini:31%(1h30m)"), "{rendered}");
    }

    #[test]
    fn dashboard_sorts_filters_and_surfaces_auth_failures() {
        let now = Utc::now();
        let mut ready =
            AccountConfig::new("zeta".parse().expect("id"), ProviderKind::Codex, "Ready");
        ready.priority = 30;
        let mut failed = AccountConfig::new(
            "alpha".parse().expect("id"),
            ProviderKind::Claude,
            "Needs login",
        );
        failed.priority = 10;

        let mut failed_status = AccountStatus::empty(failed.id.clone());
        failed_status.set_probe_failure(ProbeFailure::authentication(
            "account 'alpha' is not logged in",
        ));

        let accounts = [ready, failed];
        let statuses = [failed_status];
        let mut state = DashboardState::default();

        let visible = state.visible_accounts(&accounts, &statuses, now);
        assert_eq!(
            visible.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["zeta", "alpha"]
        );

        assert_eq!(
            state.handle_key(KeyCode::Char('s'), KeyModifiers::NONE, 2),
            DashAction::None
        );
        let visible = state.visible_accounts(&accounts, &statuses, now);
        assert_eq!(
            visible.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );

        state.handle_key(KeyCode::Char('f'), KeyModifiers::NONE, 2);
        let visible = state.visible_accounts(&accounts, &statuses, now);
        assert_eq!(
            visible.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["alpha"]
        );

        let backend = TestBackend::new(WIDE_TEST_TERM_WIDTH, TEST_TERM_HEIGHT);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| state.render(frame, &accounts, &statuses, now))
            .expect("draw frame");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(rendered.contains("AUTH"), "{rendered}");
        assert!(rendered.contains("Probe error:"), "{rendered}");
        assert!(rendered.contains("1 failed"), "{rendered}");
        assert!(rendered.contains("sort: priority"), "{rendered}");
        assert!(rendered.contains("filter: problems"), "{rendered}");
        assert!(!rendered.contains("zeta"), "{rendered}");
    }

    #[test]
    fn dashboard_key_navigation_and_actions() {
        let mut state = DashboardState::default();
        assert_eq!(
            state.handle_key(KeyCode::Char('j'), KeyModifiers::NONE, 2),
            DashAction::None
        );
        assert_eq!(state.selected, 1);
        assert_eq!(
            state.handle_key(KeyCode::Char('r'), KeyModifiers::NONE, 2),
            DashAction::ProbeAll
        );
        assert_eq!(
            state.handle_key(KeyCode::Char('c'), KeyModifiers::NONE, 2),
            DashAction::ClearSelectedCooldowns
        );
        assert_eq!(
            state.handle_key(KeyCode::Char('q'), KeyModifiers::NONE, 2),
            DashAction::Quit
        );
    }
}
