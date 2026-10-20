use bahaa_app_core::View;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget};

use super::centered_text;
use super::qr::Code;
use crate::hints;
use crate::theme;
use crate::theme::Pill;
use crate::ui::{Hit, Overlay, Prompt, Ui};
use crate::update;
use crate::wrap::{self, cells};

const MENU_WIDTH: u16 = 64;
const MENU_ROWS: usize = 12;
const MENU_TOP: u16 = 3;
const KEY_GAP: u16 = 2;
const COLUMN_GAP: u16 = 4;
const MARKER: &str = "▌";
const UPDATE_WIDTH: u16 = 72;
const NOTES_SHOWN: usize = 12;
const BUTTON_GAP: u16 = 2;

pub(super) fn qr(buffer: &mut Buffer, screen: Rect, view: &View) {
    buffer.set_style(screen, theme::backdrop());
    let Some(code) = view.page.qr.as_deref().and_then(Code::new) else {
        return;
    };
    let caption = view.page.headline.as_deref().unwrap_or_default();
    let fitted = code.scale(
        screen.width.saturating_sub(4),
        screen.height.saturating_sub(3),
    );
    let Some(scale) = fitted else {
        centered_text(buffer, screen, hints::QR_TOO_BIG, theme::text());
        return;
    };
    let (code_width, code_height) = (code.width(scale), code.height(scale));
    let width = code_width.max(wrap::width(caption)) + 4;
    let height = code_height + 3;
    let top = screen.height.saturating_sub(height) / 2;
    let Some(inner) = boxed(buffer, screen, width, height, hints::QR_TITLE, top) else {
        centered_text(buffer, screen, hints::QR_TOO_BIG, theme::text());
        return;
    };
    code.draw(
        buffer,
        Rect::new(
            inner.x + (inner.width - code_width) / 2,
            inner.y,
            code_width,
            code_height,
        ),
        scale,
    );
    Paragraph::new(Line::styled(caption, theme::headline()))
        .centered()
        .render(
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            buffer,
        );
}

pub(super) fn keys(buffer: &mut Buffer, screen: Rect) {
    buffer.set_style(screen, theme::backdrop());
    let key_width = hints::KEY_GROUPS
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(key, _)| wrap::width(key))
        .max()
        .unwrap_or(0)
        + KEY_GAP;
    let label_width = hints::KEY_GROUPS
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(_, label)| wrap::width(label))
        .max()
        .unwrap_or(0);
    let column = key_width + label_width;
    let columns: [Vec<usize>; 2] = [vec![0, 2], vec![1, 3]];
    let tall = |groups: &Vec<usize>| -> u16 {
        groups
            .iter()
            .map(|group| cells(hints::KEY_GROUPS[*group].1.len()) + 1)
            .sum::<u16>()
            + cells(groups.len().saturating_sub(1))
    };
    let height = columns.iter().map(tall).max().unwrap_or(0) + 4;
    let width = column * 2 + COLUMN_GAP + 6;
    let top = screen.height.saturating_sub(height) / 2;
    let Some(inner) = boxed(buffer, screen, width, height, hints::KEYS_TITLE, top) else {
        centered_text(buffer, screen, hints::KEYS_TOO_BIG, theme::text());
        return;
    };
    for (index, groups) in columns.iter().enumerate() {
        let x = inner.x + 1 + cells(index) * (column + COLUMN_GAP);
        let mut y = inner.y + 1;
        for group in groups {
            let (title, keys) = hints::KEY_GROUPS[*group];
            buffer.set_string(x, y, title, theme::headline());
            y += 1;
            for (key, label) in keys {
                buffer.set_string(x, y, key, theme::accent());
                buffer.set_string(x + key_width, y, label, theme::text());
                y += 1;
            }
            y += 1;
        }
    }
}

pub(super) fn menu(frame: &mut Frame<'_>, screen: Rect, view: &View, ui: &mut Ui) {
    let Some(Overlay::Menu(palette)) = &ui.overlay else {
        return;
    };
    let entries = palette.entries(view);
    let query = palette.query.text().to_owned();
    let typed = palette.query.cursor();
    let selected = palette.selected.min(entries.len().saturating_sub(1));
    let buffer = frame.buffer_mut();
    buffer.set_style(screen, theme::backdrop());
    let shown = entries.len().clamp(1, MENU_ROWS);
    let width = MENU_WIDTH.min(screen.width.saturating_sub(4));
    let Some(inner) = boxed(
        buffer,
        screen,
        width,
        cells(shown) + 4,
        hints::MENU_TITLE,
        MENU_TOP,
    ) else {
        return;
    };
    let prompt = Span::styled("› ", theme::headline());
    let line = if query.is_empty() {
        Line::from(vec![
            prompt,
            Span::styled(hints::MENU_PROMPT, theme::muted()),
        ])
    } else {
        Line::from(vec![prompt, Span::styled(query.as_str(), theme::text())])
    };
    buffer.set_line(inner.x, inner.y, &line, inner.width);
    buffer.set_string(
        inner.x,
        inner.y + 1,
        "─".repeat(usize::from(inner.width)),
        theme::edge(false),
    );
    let first = (selected + 1).saturating_sub(shown);
    let mut hits = Vec::new();
    if entries.is_empty() {
        buffer.set_string(inner.x + 2, inner.y + 2, hints::MENU_EMPTY, theme::muted());
    }
    for (row, (index, entry)) in entries
        .iter()
        .enumerate()
        .skip(first)
        .take(shown)
        .enumerate()
    {
        let y = inner.y + 2 + cells(row);
        let area = Rect::new(inner.x, y, inner.width, 1);
        let hit = Hit::Entry(index);
        let fill = if index == selected {
            theme::selected()
        } else if ui.hovered(&hit) {
            theme::raised()
        } else {
            Style::new()
        };
        buffer.set_style(area, fill);
        if index == selected {
            buffer.set_string(area.x, y, MARKER, theme::accent());
        }
        let (side, side_style) = entry
            .key
            .map_or((entry.kind, theme::muted()), |key| (key, theme::accent()));
        let side_width = wrap::width(side);
        let room = inner.width.saturating_sub(side_width + 4);
        let spans: Vec<Span<'_>> = entry
            .label
            .chars()
            .enumerate()
            .map(|(at, c)| {
                let style = if entry.marks.contains(&at) {
                    theme::headline()
                } else {
                    theme::text()
                };
                Span::styled(c.to_string(), style)
            })
            .collect();
        buffer.set_line(area.x + 2, y, &Line::from(spans), room);
        buffer.set_string(
            area.right().saturating_sub(side_width + 1),
            y,
            side,
            side_style,
        );
        hits.push((area, hit));
    }
    let cursor = Position::new(
        inner.x + 2 + cells(typed).min(inner.width.saturating_sub(3)),
        inner.y,
    );
    ui.hits.extend(hits);
    if ui.update.is_none() {
        frame.set_cursor_position(cursor);
    }
}

struct Dialog<'a> {
    title: &'static str,
    message: String,
    notes: Option<&'a [String]>,
    buttons: &'static [(&'static str, bool)],
}

fn dialog<'a>(prompt: &'a Prompt, spinner: &str) -> Dialog<'a> {
    let current = update::current();
    let (title, message, notes, buttons): (_, _, _, &'static [(&'static str, bool)]) = match prompt
    {
        Prompt::Offer(release) => (
            hints::UPDATE_TITLE,
            hints::offer(&release.version, &current),
            Some(release.notes.as_slice()),
            &[(hints::UPDATE_NOW, true), (hints::LATER, false)],
        ),
        Prompt::Installing(version) => (
            hints::UPDATES_TITLE,
            format!("{spinner} {}", hints::updating(version)),
            None,
            &[],
        ),
        Prompt::Installed(version) => (
            hints::UPDATES_TITLE,
            hints::updated(version),
            None,
            &[(hints::DONE, false)],
        ),
        Prompt::Latest => (
            hints::UPDATES_TITLE,
            hints::latest(&current),
            None,
            &[(hints::DONE, false)],
        ),
        Prompt::Failed(reason) => (
            hints::UPDATES_TITLE,
            hints::update_failed(reason),
            None,
            &[(hints::DONE, false)],
        ),
    };
    Dialog {
        title,
        message,
        notes,
        buttons,
    }
}

fn note_lines(notes: Option<&[String]>, room: u16) -> Vec<String> {
    match notes {
        None => Vec::new(),
        Some([]) => vec![hints::NO_NOTES.to_owned()],
        Some(notes) => notes
            .iter()
            .flat_map(|note| wrap::lines(note, room))
            .collect(),
    }
}

pub(super) fn update(buffer: &mut Buffer, screen: Rect, ui: &mut Ui, spinner: &str) {
    let Some(prompt) = &ui.update else {
        return;
    };
    let width = UPDATE_WIDTH.min(screen.width.saturating_sub(4));
    let room = width.saturating_sub(4);
    let (title, message, notes, buttons) = {
        let dialog = dialog(prompt, spinner);
        (
            dialog.title,
            wrap::lines(&dialog.message, room),
            note_lines(dialog.notes, room),
            dialog.buttons,
        )
    };
    buffer.set_style(screen, theme::backdrop());
    let shown = notes.len().min(NOTES_SHOWN);
    let first = usize::from(ui.notes_scroll).min(notes.len() - shown);
    ui.notes_scroll = cells(first);
    let notes_height = if notes.is_empty() {
        0
    } else {
        cells(shown) + 2
    };
    let buttons_height = if buttons.is_empty() { 0 } else { 2 };
    let height = cells(message.len()) + notes_height + buttons_height + 2;
    let top = screen.height.saturating_sub(height) / 2;
    let Some(inner) = boxed(buffer, screen, width, height, title, top) else {
        centered_text(buffer, screen, &message.join(" "), theme::text());
        return;
    };
    let mut y = inner.y;
    for line in &message {
        buffer.set_string(inner.x, y, line, theme::text());
        y += 1;
    }
    if !notes.is_empty() {
        buffer.set_string(inner.x, y + 1, hints::WHATS_NEW, theme::headline());
        if notes.len() > shown {
            let counter = format!("{}-{} of {}", first + 1, first + shown, notes.len());
            let x = inner.right().saturating_sub(wrap::width(&counter));
            buffer.set_string(x, y + 1, counter, theme::muted());
        }
        for (row, line) in notes.iter().skip(first).take(shown).enumerate() {
            buffer.set_string(inner.x, y + 2 + cells(row), line, theme::soft());
        }
        y += notes_height;
    }
    let mut x = inner.x;
    for (index, (label, confirm)) in buttons.iter().enumerate() {
        let text = format!("  {label}  ");
        let hit = Hit::Update(*confirm);
        let pill = if index == 0 {
            Pill::Focused
        } else if ui.hovered(&hit) {
            Pill::Hovered
        } else {
            Pill::Idle
        };
        let area = Rect::new(x, y + 1, wrap::width(&text), 1);
        buffer.set_string(area.x, area.y, &text, theme::pill(pill));
        ui.hits.push((area, hit));
        x += area.width + BUTTON_GAP;
    }
}

fn boxed(
    buffer: &mut Buffer,
    screen: Rect,
    width: u16,
    height: u16,
    title: &str,
    top: u16,
) -> Option<Rect> {
    if width > screen.width || height > screen.height {
        return None;
    }
    let y = (screen.y + top).min(screen.bottom() - height);
    let area = Rect::new(screen.x + (screen.width - width) / 2, y, width, height);
    Clear.render(area, buffer);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::edge(true))
        .style(theme::panel())
        .padding(Padding::horizontal(1))
        .title(Line::styled(format!(" {title} "), theme::strong()));
    let inner = block.inner(area);
    block.render(area, buffer);
    Some(inner)
}
