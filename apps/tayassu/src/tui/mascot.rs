use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use std::time::{Duration, Instant};

/// Terminal dimensions that leave room for both mascots and the quota table.
const MIN_WIDTH: u16 = 65;
const MIN_HEIGHT: u16 = 18;
/// Columns occupied by both mascots at rest, with extra room on wider terminals.
const FAMILY_WIDTH: u16 = 37;
const MAX_LANE_WIDTH: u16 = 47;
/// Columns reserved for the full status summary alongside the sprites.
pub(super) const FULL_SUMMARY_WIDTH: u16 = 37;
/// Height of the header with four rows of art and two borders.
pub(super) const HEADER_HEIGHT: u16 = 6;
/// Timing for the entrance and short gestures at rest.
const STRIDE_MS: u128 = 80;
const RUN_STRIDES: u128 = 14;
const RUN_MS: u128 = STRIDE_MS * RUN_STRIDES;
const GESTURE_MS: u128 = 400;

// Each character is one half-cell pixel. The adult's arched back, low snout
// and white cheek follow the reference photo, mirrored to face the entrance.
type PixelPair = [&'static str; 2];

const ADULT: Sprite = Sprite {
    resting_column: 0,
    delay_ms: 0,
    idle_period_ms: 3_800,
    body: [
        ["             rrrrrr", "          rrffffffffd"],
        ["       effffffffffffffd", "    ffaffffffffffffffdd"],
        [" pnfffffccfffffffffffdd", "nnffcccccdddffffffffd"],
    ],
    gesture: [
        ["             rrrrrr", "          rrffffffffd"],
        ["        efffffffffffffd", "      fdffffffffffffffdd"],
        ["   dfffffccfffffffffffdd", " pnffcccccddffffffffd"],
    ],
    standing: ["       d d       d d", "       h h       h h"],
    running: [
        ["      d  dd       dd d", "     h   hh       hh  h"],
        ["       dd        dd", "      h h       h h"],
        ["        d  d    d  d", "         h  hh    h"],
        ["      d  d      dd", "     h    h     h h"],
    ],
};

const PIGLET: Sprite = Sprite {
    resting_column: 25,
    delay_ms: STRIDE_MS * 2,
    idle_period_ms: 2_500,
    body: [
        ["", ""],
        ["        ss", "    tssbbbbs"],
        [" pbbabbbbbbb", "  cbbbbbbsb"],
    ],
    gesture: [
        ["", ""],
        ["    t   ss", "    sssbbbbs"],
        [" pbbsabbbbbs", "  cbbbbbbsb"],
    ],
    standing: ["    s s  s s", "    h h  h h"],
    running: [
        ["   s ss  ss", "  h  hh   hh"],
        ["    ss   ss", "    hh   hh"],
        ["     s ss s", "      hh  h"],
        ["   s s   ss", "  h   h  hh"],
    ],
};

struct Sprite {
    resting_column: u16,
    delay_ms: u128,
    idle_period_ms: u128,
    body: [PixelPair; 3],
    gesture: [PixelPair; 3],
    standing: PixelPair,
    running: [PixelPair; 4],
}

#[derive(Clone, Copy)]
enum Pose {
    Hidden,
    Running(u128),
    Standing,
    Gesture,
}

impl Sprite {
    /// Returns the current pose and the time until its next change.
    fn pose(&self, elapsed_ms: u128) -> (Pose, u128) {
        let Some(elapsed_ms) = elapsed_ms.checked_sub(self.delay_ms) else {
            return (Pose::Hidden, self.delay_ms - elapsed_ms);
        };
        if elapsed_ms < RUN_MS {
            return (
                Pose::Running(elapsed_ms / STRIDE_MS),
                STRIDE_MS - elapsed_ms % STRIDE_MS,
            );
        }
        let idle_remaining = self.idle_period_ms - (elapsed_ms - RUN_MS) % self.idle_period_ms;
        if idle_remaining <= GESTURE_MS {
            (Pose::Gesture, idle_remaining)
        } else {
            (Pose::Standing, idle_remaining - GESTURE_MS)
        }
    }

    fn render(&self, frame: &mut Frame<'_>, lane: Rect, pose: Pose) {
        let (advance, body, legs) = match pose {
            Pose::Hidden => return,
            Pose::Running(stride) => {
                let advance = u128::from(lane.width) * (RUN_STRIDES - stride) / RUN_STRIDES;
                let gait = usize::try_from(stride % self.running.len() as u128).unwrap_or_default();
                (
                    u16::try_from(advance).unwrap_or(lane.width),
                    self.body,
                    self.running[gait],
                )
            }
            Pose::Standing => (0, self.body, self.standing),
            Pose::Gesture => (0, self.gesture, self.standing),
        };
        let left = lane
            .x
            .saturating_add(self.resting_column)
            .saturating_add(advance);
        for (row, [upper, lower]) in lane.rows().zip(body.into_iter().chain([legs])) {
            for (index, column) in (left..row.right()).enumerate() {
                let top = upper.as_bytes().get(index).copied().and_then(pixel_color);
                let bottom = lower.as_bytes().get(index).copied().and_then(pixel_color);
                let cell = &mut frame.buffer_mut()[(column, row.y)];
                match (top, bottom) {
                    (Some(top), Some(bottom)) => {
                        cell.set_char('▀').set_fg(top).set_bg(bottom);
                    }
                    (Some(top), None) => {
                        cell.set_char('▀').set_fg(top);
                    }
                    (None, Some(bottom)) => {
                        cell.set_char('▄').set_fg(bottom);
                    }
                    (None, None) => {}
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct Mascot(Option<Instant>);

impl Mascot {
    pub(super) fn start() -> Self {
        Self(Some(Instant::now()))
    }

    pub(super) const fn fits(size: Size) -> bool {
        size.width >= MIN_WIDTH && size.height >= MIN_HEIGHT
    }

    pub(super) fn lane_width(content: Rect) -> u16 {
        content
            .width
            .saturating_sub(FULL_SUMMARY_WIDTH)
            .clamp(FAMILY_WIDTH, MAX_LANE_WIDTH)
    }

    pub(super) fn poll_interval(&self, size: Size, maximum: Duration) -> Duration {
        if !Self::fits(size) {
            return maximum;
        }
        self.next_frame_in()
            .map_or(maximum, |next| next.min(maximum))
    }

    fn next_frame_in(&self) -> Option<Duration> {
        let elapsed = self.0?.elapsed().as_millis();
        let next_ms = ADULT.pose(elapsed).1.min(PIGLET.pose(elapsed).1);
        Some(Duration::from_millis(
            u64::try_from(next_ms).unwrap_or(u64::MAX),
        ))
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, lane: Rect) {
        let elapsed = self.0.map(|start| start.elapsed().as_millis());
        for sprite in [&ADULT, &PIGLET] {
            let pose = elapsed.map_or(Pose::Standing, |time| sprite.pose(time).0);
            sprite.render(frame, lane, pose);
        }
    }
}

const fn pixel_color(pixel: u8) -> Option<Color> {
    Some(match pixel {
        b'f' => Color::Rgb(77, 80, 73),    // Adult's charcoal fur
        b'r' => Color::Rgb(105, 108, 99),  // Bristles
        b'd' => Color::Rgb(60, 62, 58),    // Shaded fur
        b'e' => Color::Rgb(141, 133, 108), // Adult ear
        b'b' => Color::Rgb(160, 101, 64),  // Piglet's russet fur
        b's' => Color::Rgb(119, 75, 54),   // Piglet's darker markings
        b't' => Color::Rgb(206, 145, 110), // Piglet ear
        b'p' => Color::Rgb(139, 99, 94),   // Snout
        b'n' => Color::Rgb(76, 58, 53),    // Nostril
        b'a' => Color::Rgb(220, 188, 137), // Eye
        b'c' => Color::Rgb(239, 232, 208), // Pale cheek
        b'h' => Color::Rgb(86, 82, 76),    // Hoof
        _ => return None,
    })
}
