mod mascot;

use crate::render::format_countdown;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use mascot::Mascot;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::{Frame, Terminal};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{self, Stdout};
use std::time::Duration;
use tayassu_core::{AccountConfig, AccountId, AccountStatus, ModelBucket};
use tayassu_store::RuntimeState;

/// Poll interval in milliseconds for live cooldown countdown ticks in the TUI.
const EVENT_POLL_MS: u64 = 500;

/// Compact header height in terminal rows.
const HEADER_HEIGHT: u16 = 3;

/// Width of the quota bar, in terminal cells.
const QUOTA_BAR_WIDTH: usize = 10;
/// Maximum number of expanded entries, keeping other accounts visible.
const QUOTA_PAGE_SIZE: usize = 5;
/// Quota below this percentage is displayed in red.
const QUOTA_CRITICAL_PERCENT: u8 = 10;
/// Quota below this percentage is displayed in orange.
const QUOTA_LOW_PERCENT: u8 = 25;

/// Footer help bar height in terminal rows.
const FOOTER_HEIGHT: u16 = 1;

/// Column widths in the main accounts table.
const COL_ACCOUNT_WIDTH: u16 = 14;
const COL_PROVIDER_WIDTH: u16 = 8;
const COL_PRIORITY_WIDTH: u16 = 3;
const COL_STATE_WIDTH: u16 = 14;
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
    detail_scroll: u16,
    show_models: bool,
    scope_index: usize,
    mascot: Mascot,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            selected: 0,
            status_message: "Ready".to_owned(),
            sort_mode: SortMode::Configured,
            filter_mode: FilterMode::All,
            detail_scroll: 0,
            show_models: false,
            scope_index: 0,
            mascot: Mascot::default(),
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
                self.detail_scroll = 0;
                self.scope_index = 0;
                DashAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev(account_count);
                self.detail_scroll = 0;
                self.scope_index = 0;
                DashAction::None
            }
            KeyCode::PageDown => {
                self.detail_scroll = self
                    .detail_scroll
                    .saturating_add(u16::try_from(QUOTA_PAGE_SIZE).unwrap_or(u16::MAX));
                DashAction::None
            }
            KeyCode::PageUp => {
                self.detail_scroll = self
                    .detail_scroll
                    .saturating_sub(u16::try_from(QUOTA_PAGE_SIZE).unwrap_or(u16::MAX));
                DashAction::None
            }
            KeyCode::Char('g') => {
                self.scope_index = self.scope_index.wrapping_add(1);
                self.detail_scroll = 0;
                DashAction::None
            }
            KeyCode::Char('m') => {
                self.show_models = !self.show_models;
                self.detail_scroll = 0;
                DashAction::None
            }
            KeyCode::Char('r') => DashAction::ProbeAll,
            KeyCode::Char('c') => DashAction::ClearSelectedCooldowns,
            KeyCode::Char('s') => {
                self.sort_mode = self.sort_mode.next();
                self.selected = 0;
                self.detail_scroll = 0;
                DashAction::None
            }
            KeyCode::Char('f') => {
                self.filter_mode = self.filter_mode.next();
                self.selected = 0;
                self.detail_scroll = 0;
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
        let area = frame.area();
        let header_height = if Mascot::fits(area.as_size()) {
            mascot::HEADER_HEIGHT
        } else {
            HEADER_HEIGHT
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_height),
                Constraint::Min(0),
                Constraint::Length(FOOTER_HEIGHT),
            ])
            .split(frame.area());
        render_header(frame, chunks[0], accounts, statuses, now, &self.mascot);
        self.render_table(frame, chunks[1], &visible, statuses, now);
        self.render_footer(frame, chunks[2]);
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
            .enumerate()
            .map(|(index, account)| {
                let status = statuses.iter().find(|entry| entry.account_id == account.id);
                let (state_text, state_color) =
                    account_state_cell(account, status, &default_bucket, now);
                let mut quota_lines: Vec<Line<'_>> = summarize_buckets(status, now)
                    .into_iter()
                    .take(2)
                    .map(Line::from)
                    .collect();
                if index == self.selected {
                    if let Some(failure) = status.and_then(|st| st.last_probe_error.as_ref()) {
                        quota_lines.push(Line::styled(
                            format!("Probe error: {}", failure.message),
                            Style::default().fg(Color::Red),
                        ));
                    }
                    let entries = quota_view_lines(status, now, self.show_models, self.scope_index);
                    let max_lines = usize::from(area.height.saturating_sub(7))
                        .saturating_sub(quota_lines.len())
                        .clamp(1, QUOTA_PAGE_SIZE);
                    let offset = usize::from(self.detail_scroll)
                        .min(entries.len().saturating_sub(1) / max_lines * max_lines);
                    let end = (offset + max_lines).min(entries.len());
                    let scope = status.and_then(|st| {
                        model_scopes(st)
                            .get(self.scope_index % model_scopes(st).len().max(1))
                            .cloned()
                    });
                    let title = if self.show_models {
                        scope.map_or_else(|| "Models".to_owned(), |name| format!("Models: {name}"))
                    } else {
                        "Quota windows".to_owned()
                    };
                    quota_lines.push(Line::styled(
                        format!(
                            "  {title} {}–{end}/{} · m switch",
                            offset + 1,
                            entries.len()
                        ),
                        Style::default().fg(Color::Cyan),
                    ));
                    quota_lines.extend(entries.into_iter().skip(offset).take(max_lines));
                }
                let quota_height = u16::try_from(quota_lines.len()).unwrap_or(u16::MAX);
                Row::new([
                    Cell::from(account.id.as_str().to_owned()),
                    Cell::from(account.provider.as_str().to_owned()),
                    Cell::from(account.priority.to_string()),
                    Cell::from(state_text).style(Style::default().fg(state_color)),
                    Cell::from(Text::from(quota_lines)),
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
            .block(panel(title, true))
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

    fn render_footer(&self, frame: &mut Frame<'_>, area: ratatui::layout::Rect) {
        let footer = Paragraph::new(Line::from(vec![
            Span::styled(
                " j/k ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("account  "),
            Span::styled(
                "PgUp/PgDn ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("page  "),
            Span::styled(
                "m ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("view  "),
            Span::styled(
                "g ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("scope  "),
            Span::styled(
                "s/f ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("sort/filter  "),
            Span::styled(
                "r ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("probe  "),
            Span::styled(
                "c ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("clear  "),
            Span::styled(
                "q ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("quit  ·  "),
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
    state_file: &std::path::Path,
    on_probe_all: FProbe,
    mut on_clear_cooldowns: FClear,
) -> Result<()>
where
    FProbe: Fn(&mut RuntimeState) -> Result<String> + Send + Sync + 'static,
    FClear: FnMut(&mut RuntimeState, &AccountId) -> Result<String>,
{
    let mut guard = TerminalGuard::enter()?;
    let mut ui = DashboardState {
        mascot: Mascot::start(),
        ..DashboardState::default()
    };
    let probe = std::sync::Arc::new(on_probe_all);
    let mut pending: Option<std::sync::mpsc::Receiver<Result<String>>> = None;

    loop {
        if let Some(receiver) = &pending {
            match receiver.try_recv() {
                Ok(result) => {
                    match RuntimeState::load(state_file) {
                        Ok(updated) => *runtime_state = updated,
                        Err(err) => {
                            ui.set_status(format!("State reload error: {err}"));
                            pending = None;
                            continue;
                        }
                    }
                    match result {
                        Ok(summary) => ui.set_status(summary),
                        Err(err) => ui.set_status(format!("Probe error: {err:#}")),
                    }
                    pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    ui.set_status("Probe worker stopped unexpectedly");
                    pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let now = Utc::now();
        if pending.is_some() {
            const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let tick =
                usize::try_from(now.timestamp_millis().unsigned_abs() / EVENT_POLL_MS).unwrap_or(0);
            ui.set_status(format!(
                "{} Probing accounts",
                SPINNER[tick % SPINNER.len()]
            ));
        }
        let visible = ui.visible_accounts(accounts, &runtime_state.statuses, now);
        ui.clamp_selection(visible.len());

        let drawn = guard
            .terminal
            .draw(|frame| ui.render(frame, accounts, &runtime_state.statuses, now))
            .context("failed to draw TUI frame")?;

        let poll_interval = ui
            .mascot
            .poll_interval(drawn.area.as_size(), Duration::from_millis(EVENT_POLL_MS));
        if event::poll(poll_interval).context("failed to poll key events")?
            && let Event::Key(key) = event::read().context("failed to read key event")?
            && key.kind == KeyEventKind::Press
        {
            match ui.handle_key(key.code, key.modifiers, visible.len()) {
                DashAction::None => {}
                DashAction::Quit => break,
                DashAction::ProbeAll => {
                    if pending.is_some() {
                        ui.set_status("Probe already in progress");
                    } else {
                        ui.set_status("Probing all accounts...");
                        let mut snapshot = runtime_state.clone();
                        let worker = std::sync::Arc::clone(&probe);
                        let (sender, receiver) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let _ = sender.send(worker(&mut snapshot));
                        });
                        pending = Some(receiver);
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

fn panel(title: String, active: bool) -> Block<'static> {
    Block::default()
        .title(title)
        .title_style(
            Style::default()
                .fg(if active { Color::Cyan } else { Color::White })
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if active { Color::Cyan } else { Color::DarkGray }))
}

fn render_header(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    accounts: &[AccountConfig],
    statuses: &[AccountStatus],
    now: DateTime<Utc>,
    mascot: &Mascot,
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

    let mut alerts = format!("{failed} failed");
    if disabled > 0 {
        let _ = write!(alerts, "  •  {disabled} disabled");
    }
    let title = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let block = panel(" Overview ".to_owned(), false);
    let content = block.inner(area);
    frame.render_widget(block, area);
    if area.height >= mascot::HEADER_HEIGHT {
        let [lane, summary] = Layout::horizontal([
            Constraint::Length(Mascot::lane_width(content)),
            Constraint::Min(0),
        ])
        .areas(content);
        mascot.render(frame, lane);
        let version = Span::styled(concat!("tayassu v", env!("CARGO_PKG_VERSION")), title);
        let lines = if summary.width >= mascot::FULL_SUMMARY_WIDTH {
            vec![
                Line::from(vec![
                    version,
                    Span::raw(format!("  ·  {}", now.format("%H:%M:%S UTC"))),
                ]),
                Line::raw(format!(
                    "{total} accounts  ·  {ready} ready  ·  {partial} partial"
                )),
                Line::raw(format!("{unknown} unknown  ·  {unavailable} unavailable")),
                Line::raw(alerts),
            ]
        } else {
            let mut accounts_line = format!("{total} total · {ready} ready");
            if disabled > 0 {
                let _ = write!(accounts_line, " · {disabled} off");
            }
            vec![
                Line::from(version),
                Line::raw(accounts_line),
                Line::raw(format!("{partial} partial · {unknown} unknown")),
                Line::raw(format!("{unavailable} unavailable · {failed} failed")),
            ]
        };
        frame.render_widget(Paragraph::new(lines), summary);
    } else {
        let line = Line::from(vec![
            Span::styled(" tayassu ", title),
            Span::raw(format!(
                "{total} accounts  ·  {ready} ready  ·  {partial} partial  ·  {unknown} unknown  ·  {unavailable} unavailable  ·  {alerts}"
            )),
        ]);
        frame.render_widget(Paragraph::new(line), content);
    }
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
    if status.is_none_or(|st| {
        st.windows.iter().all(|window| {
            window.remaining.is_none() || window.resets_at.is_some_and(|reset| reset <= now)
        })
    }) {
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

fn model_visible(status: &AccountStatus, model: &ModelBucket) -> bool {
    model
        .as_str()
        .split_once('/')
        .is_none_or(|(provider, name)| {
            name.ends_with(":free")
                || name == "free"
                || status.windows.iter().any(|window| {
                    window.window_name == "auth" && window.bucket.as_str() == provider
                })
        })
}

fn model_scopes(status: &AccountStatus) -> Vec<ModelBucket> {
    status
        .model_buckets
        .iter()
        .filter(|(model, _)| model_visible(status, model))
        .map(|(_, scope)| scope.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn quota_view_lines(
    status: Option<&AccountStatus>,
    now: DateTime<Utc>,
    show_models: bool,
    scope_index: usize,
) -> Vec<Line<'static>> {
    let Some(status) = status else {
        return vec![Line::from("  No quota data · press r")];
    };
    let mut entries = Vec::new();
    if show_models {
        let scopes = model_scopes(status);
        let selected_scope = scopes.get(scope_index % scopes.len().max(1));
        for (model, scope) in &status.model_buckets {
            if Some(scope) == selected_scope && model_visible(status, model) {
                entries.push(quota_line(
                    model.as_str(),
                    status.lowest_remaining_percent(model, now),
                    status.active_cooldown(model, now).is_some(),
                    None,
                ));
            }
        }
        if entries.is_empty() {
            entries.push(Line::from("  No model catalog · press r"));
        }
    } else {
        for window in status
            .windows
            .iter()
            .filter(|window| window.remaining.is_some())
        {
            let expired = window.resets_at.is_some_and(|reset| reset <= now);
            let reset = window.resets_at.map(|time| {
                if expired {
                    "expired".to_owned()
                } else {
                    format!("reset {}", time.format("%m-%d %H:%M UTC"))
                }
            });
            entries.push(quota_line(
                &format!("{}/{}", window.bucket, window.window_name),
                if expired { None } else { window.remaining },
                window.is_exhausted_at(now),
                reset.as_deref(),
            ));
        }
        if entries.is_empty() {
            entries.push(Line::from("  No measured quota · press r"));
        }
    }
    entries
}

fn quota_line(
    name: &str,
    remaining: Option<tayassu_core::RemainingPercent>,
    blocked: bool,
    reset: Option<&str>,
) -> Line<'static> {
    let (bar, mut color, label) = remaining.map_or_else(
        || {
            (
                "░".repeat(QUOTA_BAR_WIDTH),
                Color::Gray,
                "unknown".to_owned(),
            )
        },
        |value| {
            let pct = value.value();
            let filled = usize::from(pct) * QUOTA_BAR_WIDTH
                / usize::from(tayassu_core::MAX_REMAINING_PERCENT);
            let bar = format!(
                "{}{}",
                "█".repeat(filled),
                "░".repeat(QUOTA_BAR_WIDTH - filled)
            );
            let color = if pct < QUOTA_CRITICAL_PERCENT {
                Color::Red
            } else if pct < QUOTA_LOW_PERCENT {
                Color::Rgb(255, 165, 0)
            } else {
                Color::Green
            };
            (bar, color, value.to_string())
        },
    );
    if blocked {
        color = Color::Red;
    }
    let suffix = reset.map_or_else(String::new, |text| format!(" · {text}"));
    Line::from(vec![
        Span::styled(format!("  {bar} {label} "), Style::default().fg(color)),
        Span::raw(format!("{name}{suffix}")),
    ])
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
    desired
        .max(available)
        .min(available.max(COL_QUOTA_MIN_WIDTH))
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
            let label = st.quota_label(bucket, now);
            let pct =
                if st.last_probe_error.is_some() && !label.starts_with('~') && label != "reset✓" {
                    format!("{label} stale")
                } else {
                    label
                };
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
        status.last_probed_at = Some(now);
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
        status.last_probed_at = Some(now);

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

        failed_status.last_probed_at = Some(now);
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
    fn quota_bars_show_threshold_colors() {
        let now = Utc::now();
        let mut status = AccountStatus::empty("test".parse().expect("id"));
        status.last_probed_at = Some(now);
        for (name, percent) in [("critical", 9), ("low", 24), ("healthy", 25)] {
            status.windows.push(QuotaWindow {
                window_name: "weekly".to_owned(),
                bucket: name.parse().expect("model"),
                remaining: Some(RemainingPercent::try_from(percent).expect("percent")),
                resets_at: None,
            });
        }
        let lines = quota_view_lines(Some(&status), now, false, 0);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Rgb(255, 165, 0)));
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn quota_windows_show_each_period_and_catalog_is_paged() {
        let now = Utc::now();
        let mut status = AccountStatus::empty("test".parse().expect("id"));
        status.last_probed_at = Some(now);
        for (name, pct) in [("weekly", TEST_GEMINI_PCT), ("5h", TEST_CLAUDE_PCT)] {
            status.windows.push(QuotaWindow {
                window_name: name.to_owned(),
                bucket: "gemini".parse().expect("bucket"),
                remaining: Some(RemainingPercent::try_from(pct).expect("percent")),
                resets_at: None,
            });
        }
        for i in 0..(QUOTA_PAGE_SIZE * 3) {
            status.model_buckets.insert(
                format!("model-{i}").parse().expect("model"),
                "gemini".parse().expect("scope"),
            );
        }
        let lines = quota_view_lines(Some(&status), now, false, 0);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].to_string().contains("gemini/weekly"));
        assert!(lines[1].to_string().contains("gemini/5h"));
        let mut dashboard = DashboardState::default();
        dashboard.handle_key(KeyCode::Char('m'), KeyModifiers::NONE, 1);
        assert!(dashboard.show_models);
        dashboard.handle_key(KeyCode::PageDown, KeyModifiers::NONE, 1);
        assert_eq!(usize::from(dashboard.detail_scroll), QUOTA_PAGE_SIZE);
        let account = AccountConfig::new("test".parse().expect("id"), ProviderKind::Agy, "");
        let mut terminal =
            Terminal::new(TestBackend::new(TEST_TERM_WIDTH, TEST_TERM_HEIGHT)).expect("terminal");
        terminal
            .draw(|frame| dashboard.render(frame, &[account], &[status], now))
            .expect("draw");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("Models: gemini 6–10/15"), "{rendered}");
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
