use std::sync::LazyLock;

use bahaa_app_core::{Color as Brand, Level, Tone};
use ratatui::style::{Color, Modifier, Style};

type Rgb = [u8; 3];

const BASE: Rgb = [0x10, 0x13, 0x1a];
const PANEL: Rgb = [0x17, 0x1b, 0x25];
const RAISED: Rgb = [0x21, 0x27, 0x34];
const SELECTED: Rgb = [0x2a, 0x33, 0x48];
const EDGE: Rgb = [0x2f, 0x37, 0x4a];
const TEXT: Rgb = [0xdf, 0xe4, 0xee];
const SOFT: Rgb = [0xa3, 0xac, 0xbf];
const MUTED: Rgb = [0x6b, 0x74, 0x89];
const FADED: Rgb = [0x45, 0x4c, 0x5e];
const ACCENT: Rgb = [0x61, 0xaf, 0xef];
const ACCENT_DEEP: Rgb = [0x16, 0x2c, 0x45];
const GOOD: Rgb = [0x5f, 0xd3, 0x8d];
const WARNING: Rgb = [0xf2, 0xb8, 0x4b];
const BAD: Rgb = [0xf2, 0x6d, 0x78];
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

pub(crate) const QR_DARK: Color = Color::Indexed(16);
pub(crate) const QR_LIGHT: Color = Color::Indexed(231);

static TRUECOLOR: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("COLORTERM").is_ok_and(|depth| depth == "truecolor" || depth == "24bit")
});

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pill {
    Idle,
    Hovered,
    Chosen,
    Primary,
    Focused,
}

fn paint(rgb: Rgb) -> Color {
    let [red, green, blue] = rgb;
    if *TRUECOLOR {
        Color::Rgb(red, green, blue)
    } else {
        Color::Indexed(nearest(rgb))
    }
}

fn nearest(rgb: Rgb) -> u8 {
    let level = |channel: u8| {
        (0u8..6)
            .min_by_key(|level| CUBE[usize::from(*level)].abs_diff(channel))
            .unwrap_or(0)
    };
    let [red, green, blue] = rgb.map(level);
    let cube = [red, green, blue].map(|level| CUBE[usize::from(level)]);
    let average = rgb.iter().map(|channel| u16::from(*channel)).sum::<u16>() / 3;
    let step = u8::try_from(average.saturating_sub(8) / 10)
        .unwrap_or(23)
        .min(23);
    let gray = 8 + step * 10;
    let distance = |other: Rgb| -> u32 {
        rgb.iter()
            .zip(other)
            .map(|(a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum()
    };
    if distance([gray; 3]) < distance(cube) {
        232 + step
    } else {
        16 + 36 * red + 6 * green + blue
    }
}

pub(crate) fn base() -> Style {
    Style::new().fg(paint(TEXT)).bg(paint(BASE))
}

pub(crate) fn panel() -> Style {
    Style::new().fg(paint(TEXT)).bg(paint(PANEL))
}

pub(crate) fn backdrop() -> Style {
    Style::new().fg(paint(FADED)).bg(paint(BASE))
}

pub(crate) fn text() -> Style {
    Style::new().fg(paint(TEXT))
}

pub(crate) fn soft() -> Style {
    Style::new().fg(paint(SOFT))
}

pub(crate) fn muted() -> Style {
    Style::new().fg(paint(MUTED))
}

pub(crate) fn accent() -> Style {
    Style::new().fg(paint(ACCENT))
}

pub(crate) fn strong() -> Style {
    text().add_modifier(Modifier::BOLD)
}

pub(crate) fn headline() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

pub(crate) fn edge(focused: bool) -> Style {
    Style::new().fg(paint(if focused { ACCENT } else { EDGE }))
}

pub(crate) fn hovered_edge() -> Style {
    Style::new().fg(paint(MUTED))
}

pub(crate) fn raised() -> Style {
    Style::new().bg(paint(RAISED))
}

pub(crate) fn selected() -> Style {
    Style::new().bg(paint(SELECTED))
}

pub(crate) fn key() -> Style {
    Style::new()
        .fg(paint(ACCENT))
        .bg(paint(RAISED))
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn pill(pill: Pill) -> Style {
    match pill {
        Pill::Idle => Style::new().fg(paint(SOFT)).bg(paint(RAISED)),
        Pill::Hovered => Style::new().fg(paint(TEXT)).bg(paint(SELECTED)),
        Pill::Chosen | Pill::Primary => Style::new()
            .fg(paint(ACCENT))
            .bg(paint(ACCENT_DEEP))
            .add_modifier(Modifier::BOLD),
        Pill::Focused => Style::new()
            .fg(paint(BASE))
            .bg(paint(ACCENT))
            .add_modifier(Modifier::BOLD),
    }
}

pub(crate) fn tone(tone: Tone) -> Style {
    Style::new().fg(paint(match tone {
        Tone::Plain => TEXT,
        Tone::Muted => SOFT,
        Tone::Good => GOOD,
        Tone::Waiting => ACCENT,
        Tone::Warning => WARNING,
        Tone::Bad => BAD,
        Tone::Brand(color) => brand(color),
    }))
}

fn brand(color: Brand) -> Rgb {
    [color.red, color.green, color.blue]
}

pub(crate) fn level(level: Level) -> Style {
    Style::new().fg(paint(match level {
        Level::Info => ACCENT,
        Level::Success => GOOD,
        Level::Warning => WARNING,
        Level::Error => BAD,
    }))
}

pub(crate) fn icon(level: Level) -> &'static str {
    match level {
        Level::Info => "●",
        Level::Success => "✓",
        Level::Warning => "▲",
        Level::Error => "✗",
    }
}
