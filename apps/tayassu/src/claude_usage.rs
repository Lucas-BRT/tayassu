use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};
use tayassu_core::{MAX_REMAINING_PERCENT, ModelBucket, QuotaWindow, RemainingPercent};

/// Terminal dimensions that keep labels and reset times on a single rendered line.
const ROWS: u16 = 60;
const COLS: u16 = 120;
/// Budget for startup and the Claude Code usage request.
const DEADLINE: Duration = Duration::from_secs(12);

/// Let the async usage panel settle before reading all visible windows.
const SCREEN_SETTLE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy)]
enum Phase {
    Prompt,
    Menu,
    Panel,
}

pub(crate) fn probe(
    executable: &Path,
    environment: &[(String, String)],
    now: DateTime<Utc>,
) -> Result<Vec<QuotaWindow>> {
    let pair = native_pty_system().openpty(PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let cwd = tempfile::tempdir().context("failed to create isolated Claude probe directory")?;
    let mut command = CommandBuilder::new(executable);
    command.cwd(cwd.path());
    command.args([
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        r#"{"mcpServers":{}}"#,
        "--settings",
        r#"{"disableAllHooks":true}"#,
        "--tools",
        "",
    ]);
    for (key, value) in environment {
        command.env(key, value);
    }
    command.env("TERM", "xterm-256color");
    command.env("LANG", "C.UTF-8");
    command.env("TZ", "UTC");
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);

    let (sender, receiver) = sync_channel(16);
    std::thread::spawn(move || {
        let mut buffer = [0; 8192];
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 || sender.send(buffer[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    let result = (|| {
        let start = Instant::now();
        let mut terminal = vt100::Parser::new(ROWS, COLS, 0);
        let mut phase = Phase::Prompt;
        while let Some(time_left) = DEADLINE.checked_sub(start.elapsed()) {
            let wait = if matches!(phase, Phase::Panel) {
                time_left.min(SCREEN_SETTLE)
            } else {
                time_left
            };
            match receiver.recv_timeout(wait) {
                Ok(bytes) => terminal.process(&bytes),
                Err(RecvTimeoutError::Timeout) if matches!(phase, Phase::Panel) => {
                    if let Some(windows) = parse_screen(&terminal.screen().contents(), now)? {
                        return Ok(windows);
                    }
                    continue;
                }
                Err(_) => break,
            }
            let screen = terminal.screen().contents();
            match phase {
                Phase::Prompt
                    if screen.lines().any(|line| line.trim() == "❯")
                        && !screen.contains("Do you trust") =>
                {
                    writer.write_all(b"/usage")?;
                    writer.flush()?;
                    phase = Phase::Menu;
                }
                Phase::Menu if confirms_usage_command(&screen) => {
                    writer.write_all(b"\r")?;
                    writer.flush()?;
                    phase = Phase::Panel;
                }
                _ => {}
            }
        }
        bail!("Claude Code did not return an identifiable plan-usage screen")
    })();

    let _ = child.kill();
    child
        .wait()
        .context("failed to reap Claude Code usage probe")?;
    result
}

fn confirms_usage_command(screen: &str) -> bool {
    screen.lines().map(str::trim_start).any(|line| {
        line.strip_prefix("❯ /usage").is_some_and(|description| {
            description.starts_with(char::is_whitespace)
                && description
                    .split_whitespace()
                    .take(3)
                    .eq(["Show", "session", "cost,"])
        })
    })
}

fn parse_screen(screen: &str, now: DateTime<Utc>) -> Result<Option<Vec<QuotaWindow>>> {
    if screen.contains("Showing last-known usage") {
        bail!("Claude Code reported a cached usage snapshot");
    }

    let mut windows = Vec::new();
    let mut current = None;
    for line in screen.lines().map(str::trim) {
        let label = line.to_ascii_lowercase();
        if label.starts_with("current session") || label.starts_with("current week") {
            let bucket = if label.contains("opus") {
                "opus".parse()?
            } else if label.contains("sonnet") {
                "sonnet".parse()?
            } else if label.contains("haiku") {
                "haiku".parse()?
            } else if !label.contains('(') || label.contains("all models") {
                ModelBucket::default_bucket()
            } else {
                continue;
            };
            current = Some((label, bucket, None));
        }
        let Some((name, bucket, reading)) = current.as_mut() else {
            continue;
        };
        if let Some((remaining, unit)) = parse_percentage(line) {
            *reading = Some(if unit == "used" {
                MAX_REMAINING_PERCENT - remaining
            } else {
                remaining
            });
        }
        if let (Some(remaining), Some(reset)) = (*reading, line.strip_prefix("Resets ")) {
            let resets_at = parse_reset(reset, now).context("unknown Claude Code reset format")?;
            windows.push(QuotaWindow {
                window_name: name.clone(),
                bucket: bucket.clone(),
                remaining: Some(RemainingPercent::try_from(remaining)?),
                resets_at: Some(resets_at),
            });
            current = None;
        }
    }

    Ok((!windows.is_empty() && current.is_none()).then_some(windows))
}

fn parse_percentage(line: &str) -> Option<(u8, &str)> {
    let (before, after) = line.split_once('%')?;
    let number = before.split_whitespace().next_back()?.parse::<u8>().ok()?;
    let unit = after.split_whitespace().next()?;
    (number <= MAX_REMAINING_PERCENT && matches!(unit, "used" | "left" | "remaining"))
        .then_some((number, unit))
}

fn parse_reset(raw: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let value = raw.strip_suffix("(UTC)")?.trim();
    let lower = value.to_ascii_lowercase();
    if let Some((date, time)) = lower.split_once(',') {
        for year in [now.year(), now.year().checked_add(1)?] {
            let date = NaiveDate::parse_from_str(&format!("{year} {date}"), "%Y %b %e").ok()?;
            let time = chrono::NaiveTime::parse_from_str(time.trim(), "%l:%M%P").ok()?;
            let reset = date.and_time(time).and_utc();
            if reset > now {
                return Some(reset);
            }
        }
        None
    } else {
        let time = chrono::NaiveTime::parse_from_str(&lower, "%l:%M%P").ok()?;
        let same_day = now.date_naive().and_time(time).and_utc();
        Some(if same_day > now {
            same_day
        } else {
            same_day + chrono::TimeDelta::days(1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_shared_and_family_limits_with_both_percentage_directions() {
        let now = "2026-12-31T23:00:00Z".parse().expect("test time");
        let screen = "\
Current session
████                 20% used
Resets 1:30am (UTC)
Current week (all models)
████                 65% used
Resets Jan 2, 7:59am (UTC)
Current week (Opus)
███                  5% remaining
Resets Jan 2, 7:59am (UTC)";
        let windows = parse_screen(screen, now)
            .expect("parse usage")
            .expect("windows");
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].remaining.map(RemainingPercent::value), Some(80));
        assert_eq!(windows[1].remaining.map(RemainingPercent::value), Some(35));
        assert_eq!(windows[2].bucket.as_str(), "opus");
        assert_eq!(windows[2].remaining.map(RemainingPercent::value), Some(5));
        assert_eq!(
            windows[2].resets_at,
            Some("2027-01-02T07:59:00Z".parse().expect("reset"))
        );
    }

    #[test]
    fn rejects_stale_panel_and_confirms_only_builtin_slash_entry() {
        let now = Utc::now();
        assert!(parse_screen("Showing last-known usage from 5 minutes ago", now).is_err());
        assert!(!confirms_usage_command("❯ /usage"));
        assert!(confirms_usage_command(
            "  ❯ /usage    Show session cost, plan usage, and activity stats"
        ));
    }
}
