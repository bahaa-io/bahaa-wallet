use bahaa_app_core::{CHART_LEVELS, Chart, Glance};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Widget};

use super::parts::Sink;
use crate::theme::{self, Pill};
use crate::ui::{Hit, Target, Ui};
use crate::wrap;

pub(crate) const HEIGHT: u16 = 11;
const ROWS: u16 = 6;
const MARK_ROOM: u16 = 16;
const MAX_MARKS: u16 = 6;
const FILL_TOP: u32 = 300;
const FILL_FADE: u32 = 220;
const LIT: u32 = 220;

pub(crate) fn draw(buffer: &mut Buffer, area: Rect, chart: &Chart, ui: &mut Ui, sink: &mut Sink) {
    if area.width < 8 || area.height < HEIGHT {
        return;
    }
    let plot = Rect::new(area.x + 1, area.y + 3, area.width - 2, ROWS);
    let focused = ui.focus == Some(Target::Chart);
    sink.hits.push((
        Rect::new(plot.x, area.y + 2, plot.width, HEIGHT - 2),
        Hit::Focus(Target::Chart),
    ));
    if focused {
        sink.focus = Some(area);
    }
    ui.plot = plot.width;
    let count = chart.points.len();
    let cursor = ui
        .pointing
        .or(ui.point.filter(|_| focused))
        .filter(|_| count > 0)
        .map(|column| column.min(plot.width - 1));
    let pointed = cursor.map(|column| (column, spread(column, plot.width, count)));
    let glance = pointed.map_or(&chart.glance, |(_, index)| &chart.points[index].glance);
    header(buffer, area, chart, glance, pointed.is_some(), ui, sink);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::edge(false))
        .render(Rect::new(area.x, area.y + 2, area.width, ROWS + 2), buffer);
    if count == 0 {
        if let Some(note) = &chart.note {
            let width = wrap::width(note).min(plot.width);
            let x = plot.x + (plot.width - width) / 2;
            buffer.set_stringn(x, plot.y + 2, note, usize::from(plot.width), theme::muted());
        }
        return;
    }
    curve(buffer, plot, chart, glance, cursor);
    for (index, y) in [(chart.high, area.y + 2), (chart.low, plot.bottom())] {
        let text = format!(" {} ", chart.points[index].glance.price);
        let x = place(plot.x + spot(index, plot.width, count), &text, plot);
        buffer.set_string(x, y, &text, theme::soft());
    }
    axis(buffer, area.bottom() - 1, plot, chart, pointed);
}

fn header(
    buffer: &mut Buffer,
    area: Rect,
    chart: &Chart,
    glance: &Glance,
    pointed: bool,
    ui: &Ui,
    sink: &mut Sink,
) {
    let labels: Vec<String> = chart
        .spans
        .choices
        .iter()
        .map(|choice| format!(" {} ", choice.label))
        .collect();
    let mut x = area
        .right()
        .saturating_sub(labels.iter().map(|label| wrap::width(label)).sum());
    let limit = x.saturating_sub(2);
    for (choice, label) in chart.spans.choices.iter().zip(&labels) {
        let hit = Hit::Action(choice.action.clone());
        let pill = match (choice.selected, ui.focus == Some(Target::Chart)) {
            (true, true) => Pill::Focused,
            (true, false) => Pill::Chosen,
            (false, _) if ui.hovered(&hit) => Pill::Hovered,
            (false, _) => Pill::Idle,
        };
        let width = wrap::width(label);
        buffer.set_string(x, area.y, label, theme::pill(pill));
        sink.hits.push((Rect::new(x, area.y, width, 1), hit));
        x += width;
    }
    let when = if pointed {
        theme::soft()
    } else {
        theme::muted()
    };
    let parts = [
        (&glance.price, theme::strong(), 3),
        (&glance.change, theme::tone(glance.tone), 2),
        (&glance.moved, theme::tone(glance.tone), 3),
        (&glance.when, when, 0),
    ];
    let full: u16 = parts
        .iter()
        .filter(|(text, ..)| !text.is_empty())
        .map(|(text, _, gap)| wrap::width(text) + gap)
        .sum();
    let mut x = area.x;
    for (index, (text, style, gap)) in parts.into_iter().enumerate() {
        let width = wrap::width(text);
        if text.is_empty() || (index == 2 && area.x + full > limit) {
            continue;
        }
        if x + width > limit {
            break;
        }
        buffer.set_string(x, area.y, text, style);
        x += width + gap;
    }
}

fn curve(buffer: &mut Buffer, plot: Rect, chart: &Chart, glance: &Glance, cursor: Option<u16>) {
    let pixels = u32::from(ROWS) * 2;
    let levels = u32::from(CHART_LEVELS);
    let tops: Vec<u32> = (0..plot.width)
        .map(|column| {
            let point = &chart.points[spread(column, plot.width, chart.points.len())];
            let depth = levels - u32::from(point.level).min(levels);
            (depth * (pixels - 1) + levels / 2) / levels
        })
        .collect();
    for (column, x) in (0..plot.width).zip(plot.x..) {
        let here = tops[usize::from(column)];
        let (mut upper, mut lower) = (here, here);
        let before = column.checked_sub(1);
        let after = Some(column + 1).filter(|next| *next < plot.width);
        for neighbor in [before, after].into_iter().flatten() {
            let other = tops[usize::from(neighbor)];
            if other > here {
                lower = lower.max(u32::midpoint(here, other));
            } else if other < here {
                upper = upper.min((here + other).div_ceil(2));
            }
        }
        let lit = cursor == Some(column);
        let color = |pixel: u32| -> Option<Color> {
            if pixel < upper {
                None
            } else if pixel <= lower {
                Some(theme::line(glance.tone, lit))
            } else {
                let fade = FILL_TOP - FILL_FADE * (pixel / 2) / (u32::from(ROWS) - 1);
                Some(theme::fill(glance.tone, fade + if lit { LIT } else { 0 }))
            }
        };
        for (row, y) in (0..u32::from(ROWS)).zip(plot.y..) {
            let cell = &mut buffer[(x, y)];
            match (color(row * 2), color(row * 2 + 1)) {
                (None, None) => {
                    if lit {
                        cell.set_symbol("│").set_style(theme::muted());
                    }
                }
                (top, bottom) if top == bottom => {
                    cell.set_symbol(" ")
                        .set_bg(top.unwrap_or_else(theme::empty));
                }
                (top, bottom) => {
                    cell.set_symbol("▄").set_style(
                        Style::new()
                            .fg(bottom.unwrap_or_else(theme::empty))
                            .bg(top.unwrap_or_else(theme::empty)),
                    );
                }
            }
        }
    }
}

fn axis(buffer: &mut Buffer, y: u16, plot: Rect, chart: &Chart, pointed: Option<(u16, usize)>) {
    if let Some((column, index)) = pointed {
        let text = format!(" {} ", chart.points[index].glance.when);
        let x = place(plot.x + column, &text, plot);
        buffer.set_string(x, y, &text, theme::text().patch(theme::raised()));
        return;
    }
    let marks = (plot.width / MARK_ROOM).clamp(2, MAX_MARKS);
    let mut end = plot.x;
    for step in 0..marks {
        let column = step * (plot.width - 1) / (marks - 1);
        let text = &chart.points[spread(column, plot.width, chart.points.len())].mark;
        let width = wrap::width(text);
        let x = match step {
            0 => plot.x,
            _ if step + 1 == marks => plot.right().saturating_sub(width),
            _ => (plot.x + column).saturating_sub(width / 2),
        };
        if step > 0 && x < end + 2 {
            continue;
        }
        buffer.set_string(x, y, text, theme::muted());
        end = x + width;
    }
}

fn spread(column: u16, width: u16, count: usize) -> usize {
    let span = usize::from(width.saturating_sub(1)).max(1);
    ((usize::from(column) * count.saturating_sub(1) + span / 2) / span).min(count.saturating_sub(1))
}

fn spot(index: usize, width: u16, count: usize) -> u16 {
    let span = count.saturating_sub(1).max(1);
    u16::try_from(index * usize::from(width.saturating_sub(1)) / span).unwrap_or(width)
}

fn place(center: u16, text: &str, plot: Rect) -> u16 {
    let width = wrap::width(text);
    center
        .saturating_sub(width / 2)
        .max(plot.x)
        .min(plot.right().saturating_sub(width))
}
