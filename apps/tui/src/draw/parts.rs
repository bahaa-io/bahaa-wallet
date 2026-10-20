use std::ops::Range;

use bahaa_app_core::{
    self as app, Action, Choice, Field, Group, Item, Level, Page, Share, Tone, Value,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, HighlightSpacing, Padding, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, StatefulWidget, Table, Widget,
};
use unicode_width::UnicodeWidthChar;
use zeroize::Zeroizing;

use crate::editor::Editor;
use crate::hints;
use crate::theme::{self, Pill};
use crate::ui::{Hit, Target, Ui};
use crate::wrap::{self, cells};

const INPUT_WIDTH: u16 = 72;
const MAX_LABEL: u16 = 20;
const CHOICE_GAP: u16 = 1;
const BUTTON_GAP: u16 = 2;
const COLUMN_GAP: u16 = 2;
const MIN_COLUMN: u16 = 6;
const SHARE_WIDTH: u16 = 72;
const LEGEND_GAP: u16 = 3;
const PERMILLE: u16 = 1000;
const HIGHLIGHT: &str = "▌ ";
const TRACK: &str = "│";
const THUMB: &str = "┃";
const DOT: &str = "● ";
const WARN: &str = "▲";
const BAR: &str = "━";
const LIST: usize = 10;
const PHRASE_LINES: usize = 3;
const WORD_COLUMN: u16 = 16;
const MASK: char = '•';
const HIDDEN_WORD: &str = "••••••";
const KEY_GROUP: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Title,
    Text,
    Headline,
    Shares,
    Secret,
    Lines,
    Item(usize),
    Table,
    Notes,
    Error,
    Buttons,
}

#[derive(Default)]
pub(crate) struct Sink {
    pub(crate) hits: Vec<(Rect, Hit)>,
    pub(crate) cursor: Option<Position>,
    pub(crate) focus: Option<Rect>,
    pub(crate) table: Option<Rect>,
    pub(crate) tooltip: Option<String>,
}

pub(crate) struct Sizes {
    pub(crate) vertical: Vec<bool>,
    width: u16,
    label: u16,
    group_label: u16,
    buttons: Vec<(u16, u16)>,
}

impl Sizes {
    pub(crate) fn new(page: &Page, width: u16) -> Self {
        let label = column(page.lines.iter().map(|line| line.label.as_str()));
        let group_label = column(page.groups().map(|group| group.label.as_str()));
        let vertical = page
            .form
            .iter()
            .map(|item| match item {
                Item::Group(group) => {
                    wrap::width(&group.label) + 2 > group_label
                        || indent(group, group_label) + choices_width(group) > width
                }
                Item::Input(_) => false,
            })
            .collect();
        let mut place = (0, 0);
        let buttons = page
            .buttons
            .iter()
            .map(|button| {
                let size = wrap::width(&button_text(&button.label));
                if place.0 > 0 && place.0 + size > width {
                    place = (0, place.1 + 1);
                }
                let at = place;
                place.0 += size + BUTTON_GAP;
                at
            })
            .collect();
        Self {
            vertical,
            width,
            label,
            group_label,
            buttons,
        }
    }

    pub(crate) fn joined(&self, page: &Page, previous: Part, next: Part) -> bool {
        let inline = |part| match part {
            Part::Item(index) => {
                matches!(page.form[index], Item::Group(_)) && !self.vertical[index]
            }
            _ => false,
        };
        inline(previous) && inline(next)
    }
}

pub(crate) fn list(page: &Page, titled: bool) -> Vec<Part> {
    let shown = |parts: Vec<(bool, Part)>| {
        parts
            .into_iter()
            .filter_map(|(shown, part)| shown.then_some(part))
    };
    shown(vec![
        (titled, Part::Title),
        (page.text.is_some(), Part::Text),
        (page.headline.is_some(), Part::Headline),
        (!page.shares.is_empty(), Part::Shares),
        (page.secret.is_some(), Part::Secret),
        (!page.lines.is_empty(), Part::Lines),
    ])
    .chain((0..page.form.len()).map(Part::Item))
    .chain(shown(vec![
        (page.table.is_some(), Part::Table),
        (!page.notes.is_empty(), Part::Notes),
        (page.error.is_some(), Part::Error),
        (!page.buttons.is_empty(), Part::Buttons),
    ]))
    .collect()
}

pub(crate) fn height(part: Part, page: &Page, ui: &Ui, sizes: &Sizes) -> u16 {
    let width = sizes.width;
    match part {
        Part::Text => count(page.text.as_deref(), width),
        Part::Headline => count(page.headline.as_deref(), width),
        Part::Shares => 2,
        Part::Secret => page
            .secret
            .as_ref()
            .map_or(0, |secret| grid(secret.words().count(), width).1 + 2),
        Part::Lines => page
            .lines
            .iter()
            .map(|line| {
                count(
                    Some(&line.value),
                    width.saturating_sub(sizes.label + lead(line.tone)),
                )
                .max(1)
            })
            .sum(),
        Part::Item(index) => match &page.form[index] {
            Item::Group(group) if sizes.vertical[index] => 1 + cells(group.choices.len().min(LIST)),
            Item::Group(_) => 1,
            Item::Input(input) => {
                2 + cells(input_rows(input, &shown(input, ui).0, input_inner(width)))
            }
        },
        Part::Title => 1,
        Part::Table => page
            .table
            .as_ref()
            .map_or(0, |table| cells(table.rows.len().max(1)) + 4),
        Part::Notes => page
            .notes
            .iter()
            .map(|note| count(Some(&note.text), width.saturating_sub(2)))
            .sum(),
        Part::Error => count(page.error.as_deref(), width.saturating_sub(2)),
        Part::Buttons => sizes.buttons.last().map_or(0, |(_, row)| row + 1),
    }
}

pub(crate) fn draw(
    part: Part,
    area: Rect,
    buffer: &mut Buffer,
    page: &Page,
    ui: &mut Ui,
    sizes: &Sizes,
    sink: &mut Sink,
) {
    match part {
        Part::Title => paragraph(buffer, area, Some(&page.title), theme::strong()),
        Part::Text => paragraph(buffer, area, page.text.as_deref(), theme::soft()),
        Part::Headline => paragraph(buffer, area, page.headline.as_deref(), theme::headline()),
        Part::Shares => shares(buffer, area, &page.shares),
        Part::Secret => secret(buffer, area, page, ui),
        Part::Lines => lines(buffer, area, page, sizes),
        Part::Item(index) => match &page.form[index] {
            Item::Group(group) => self::group(buffer, area, group, index, ui, sizes, sink),
            Item::Input(input) => self::input(buffer, area, input, ui, sink),
        },
        Part::Table => table(buffer, area, page, ui, sink),
        Part::Notes => {
            let mut y = area.y;
            for note in &page.notes {
                y += marked(buffer, Rect { y, ..area }, note.level, &note.text);
            }
        }
        Part::Error => {
            if let Some(error) = &page.error {
                marked(buffer, area, Level::Error, error);
            }
        }
        Part::Buttons => buttons(buffer, area, page, ui, sizes, sink),
    }
}

fn paragraph(buffer: &mut Buffer, area: Rect, text: Option<&str>, style: Style) {
    for (row, line) in text
        .map(|text| wrap::lines(text, area.width))
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        buffer.set_string(area.x, area.y + cells(row), line, style);
    }
}

fn shares(buffer: &mut Buffer, area: Rect, shares: &[Share]) {
    let width = area.width.min(SHARE_WIDTH);
    let mut x = area.x;
    let mut used = 0;
    for (index, share) in shares.iter().enumerate() {
        let part = if index + 1 == shares.len() {
            width.saturating_sub(used)
        } else {
            let scaled = u32::from(share.permille) * u32::from(width) / u32::from(PERMILLE);
            u16::try_from(scaled).unwrap_or(width).max(1)
        };
        let part = part.min(width.saturating_sub(used));
        buffer.set_string(
            x,
            area.y,
            BAR.repeat(usize::from(part)),
            theme::tone(share.tone),
        );
        x += part;
        used += part;
    }
    let mut x = area.x;
    for share in shares {
        let percent = match share.permille {
            0..5 => "<1%".to_owned(),
            permille => format!("{}%", (permille + 5) / 10),
        };
        let line = Line::from(vec![
            Span::styled(DOT, theme::tone(share.tone)),
            Span::styled(share.label.as_str(), theme::text()),
            Span::raw(" "),
            Span::styled(percent, theme::muted()),
        ]);
        let item = cells(line.width());
        if x + item > area.right() {
            break;
        }
        buffer.set_line(x, area.y + 1, &line, item);
        x += item + LEGEND_GAP;
    }
}

fn written(tone: Tone) -> Style {
    match tone {
        Tone::Brand(_) => theme::text(),
        _ => theme::tone(tone),
    }
}

fn lead(tone: Tone) -> u16 {
    if matches!(tone, Tone::Brand(_)) {
        wrap::width(DOT)
    } else {
        0
    }
}

fn lines(buffer: &mut Buffer, area: Rect, page: &Page, sizes: &Sizes) {
    let mut y = area.y;
    for line in &page.lines {
        buffer.set_stringn(
            area.x,
            y,
            &line.label,
            usize::from(sizes.label - 2),
            theme::muted(),
        );
        let mut x = area.x + sizes.label;
        if lead(line.tone) > 0 {
            buffer.set_string(x, y, DOT, theme::tone(line.tone));
            x += wrap::width(DOT);
        }
        let style = written(line.tone);
        let values = wrap::lines(&line.value, area.right().saturating_sub(x));
        for (row, value) in values.iter().enumerate() {
            buffer.set_string(x, y + cells(row), value, style);
        }
        y += cells(values.len()).max(1);
    }
}

fn group(
    buffer: &mut Buffer,
    area: Rect,
    group: &Group,
    index: usize,
    ui: &Ui,
    sizes: &Sizes,
    sink: &mut Sink,
) {
    let focused = ui.focus == Some(Target::Group(index));
    let cursor = ui.cursors.get(index).copied().unwrap_or(0);
    let label = if focused {
        theme::headline()
    } else {
        theme::muted()
    };
    buffer.set_string(area.x, area.y, &group.label, label);
    let toggle = is_toggle(group);
    if sizes.vertical[index] {
        let total = group.choices.len();
        let visible = total.min(LIST);
        let start = (cursor + 1).saturating_sub(visible).min(total - visible);
        for (row, at) in (start..start + visible).enumerate() {
            let y = area.y + 1 + cells(row);
            let rect = Rect::new(area.x, y, area.width.min(INPUT_WIDTH), 1);
            let hit = Hit::Choice(index, at);
            let current = focused && at == cursor;
            let fill = if current {
                theme::selected()
            } else if ui.hovered(&hit) {
                sink.tooltip = Some(hints::pick(&group.choices[at].label));
                theme::raised()
            } else {
                Style::new()
            };
            buffer.set_style(rect, fill);
            if current {
                buffer.set_string(rect.x, y, HIGHLIGHT, theme::accent());
            }
            let choice = &group.choices[at];
            let (mark, mark_style) = if choice.selected {
                ("● ", theme::accent())
            } else {
                ("○ ", theme::muted())
            };
            let mark_style = match choice.tone {
                Tone::Brand(_) => theme::tone(choice.tone),
                _ => mark_style,
            };
            buffer.set_string(rect.x + 2, y, mark, mark_style);
            let text = if choice.selected || current {
                theme::strong()
            } else {
                theme::text()
            };
            buffer.set_stringn(
                rect.x + 4,
                y,
                &choice.label,
                usize::from(rect.width.saturating_sub(5)),
                text,
            );
            sink.hits.push((rect, hit));
        }
        if total > visible {
            buffer.set_string(
                area.x + wrap::width(&group.label) + 2,
                area.y,
                format!("{}-{} of {total}", start + 1, start + visible),
                theme::muted(),
            );
        }
    } else {
        let mut x = area.x + indent(group, sizes.group_label);
        for (at, choice) in group.choices.iter().enumerate() {
            let text = choice_text(choice, toggle);
            let hit = Hit::Choice(index, at);
            let hovered = ui.hovered(&hit);
            let pill = if focused && at == cursor {
                Pill::Focused
            } else if choice.selected {
                Pill::Chosen
            } else if hovered {
                Pill::Hovered
            } else {
                Pill::Idle
            };
            if hovered && !choice.selected {
                sink.tooltip = Some(hints::pick(&choice.label));
            }
            let room = area.right().saturating_sub(x);
            buffer.set_stringn(x, area.y, &text, usize::from(room), theme::pill(pill));
            if let Tone::Brand(_) = choice.tone
                && room > 1
            {
                buffer.set_style(Rect::new(x + 1, area.y, 1, 1), theme::tone(choice.tone));
            }
            let width = wrap::width(&text).min(room);
            sink.hits.push((Rect::new(x, area.y, width, 1), hit));
            x += width + CHOICE_GAP;
        }
    }
    if focused {
        sink.focus = Some(area);
    }
}

fn input(buffer: &mut Buffer, area: Rect, input: &app::Input, ui: &Ui, sink: &mut Sink) {
    let focused = ui.focus == Some(Target::Input(input.field));
    let area = Rect {
        width: area.width.min(INPUT_WIDTH),
        ..area
    };
    let hit = Hit::Focus(Target::Input(input.field));
    let hovered = !focused && ui.hovered(&hit);
    if hovered {
        sink.tooltip = Some(hints::CLICK_TO_TYPE.to_owned());
    }
    let (border, label) = if focused {
        (theme::edge(true), theme::headline())
    } else if hovered {
        (theme::hovered_edge(), theme::text())
    } else {
        (theme::edge(false), theme::soft())
    };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .padding(Padding::horizontal(1))
        .title(Line::styled(format!(" {} ", input.label), label));
    let suggestions = ui.suggestions(input.field);
    if !suggestions.is_empty() {
        block = block.title_bottom(Line::styled(
            format!(" {} ", suggestions.join(" · ")),
            theme::soft(),
        ));
    } else if let Some(hint) = &input.hint {
        let (text, style) = match hint.level {
            Level::Info => (format!(" {} ", hint.text), theme::soft()),
            level => (
                format!(" {} {} ", theme::icon(level), hint.text),
                theme::level(level),
            ),
        };
        block = block.title_bottom(Line::styled(text, style));
    }
    let inner = block.inner(area);
    block.render(area, buffer);
    let (text, cursor) = shown(input, ui);
    let at = if input.field == Field::Phrase {
        wrapped(buffer, inner, &text, cursor)
    } else {
        single(buffer, inner, &text, cursor)
    };
    sink.hits.push((area, hit));
    if focused {
        sink.cursor = Some(at);
        sink.focus = Some(area);
    }
}

fn shown(input: &app::Input, ui: &Ui) -> (Zeroizing<String>, usize) {
    let editor = ui.editors.get(&input.field);
    let text = editor.map_or_else(|| input.value.text(), Editor::text);
    let cursor = editor.map_or_else(|| text.chars().count(), Editor::cursor);
    let masked = matches!(input.value, Value::Hidden(_)) && ui.masked(input.field);
    let shown = if masked {
        text.chars().map(|_| MASK).collect()
    } else {
        text.to_owned()
    };
    (Zeroizing::new(shown), cursor)
}

fn input_rows(input: &app::Input, text: &str, width: u16) -> usize {
    if input.field == Field::Phrase {
        wrap::ranges(text, width).len().max(PHRASE_LINES)
    } else {
        1
    }
}

fn single(buffer: &mut Buffer, inner: Rect, text: &str, cursor: usize) -> Position {
    let room = usize::from(inner.width.max(1));
    let start = (cursor + 1).saturating_sub(room);
    let visible = slice(text, start..start + room);
    buffer.set_string(inner.x, inner.y, visible.as_str(), theme::text());
    Position::new(inner.x + cells(cursor - start), inner.y)
}

fn wrapped(buffer: &mut Buffer, inner: Rect, text: &str, cursor: usize) -> Position {
    let ranges = wrap::ranges(text, inner.width);
    for (row, range) in ranges.iter().enumerate().take(usize::from(inner.height)) {
        let line = slice(text, range.clone());
        buffer.set_string(
            inner.x,
            inner.y + cells(row),
            line.trim_end(),
            theme::text(),
        );
    }
    let row = ranges
        .iter()
        .rposition(|range| range.start <= cursor)
        .unwrap_or(0);
    let start = ranges.get(row).map_or(0, |range| range.start);
    let column: usize = text
        .chars()
        .skip(start)
        .take(cursor - start)
        .map(|c| c.width().unwrap_or(0))
        .sum();
    Position::new(
        inner.x + cells(column).min(inner.width.saturating_sub(1)),
        inner.y + cells(row).min(inner.height.saturating_sub(1)),
    )
}

fn slice(text: &str, range: Range<usize>) -> Zeroizing<String> {
    let mut part = Zeroizing::new(String::with_capacity(text.len()));
    part.extend(text.chars().skip(range.start).take(range.len()));
    part
}

fn table(buffer: &mut Buffer, area: Rect, page: &Page, ui: &mut Ui, sink: &mut Sink) {
    let Some(table) = &page.table else {
        return;
    };
    let focused = ui.focus == Some(Target::Table);
    let gutter = wrap::width(HIGHLIGHT);
    let natural = widths(table, u16::MAX);
    let wanted = natural.iter().sum::<u16>()
        + COLUMN_GAP * cells(natural.len().saturating_sub(1))
        + gutter
        + 2;
    let area = Rect {
        width: wanted.max(INPUT_WIDTH).min(area.width),
        ..area
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::edge(focused));
    let inner = block.inner(area);
    let widths = widths(table, inner.width.saturating_sub(gutter));
    let right: Vec<bool> = (0..table.columns.len())
        .map(|column| numeric(table, column))
        .collect();
    let header = Row::new(
        table
            .columns
            .iter()
            .zip(&widths)
            .zip(&right)
            .map(|((text, width), right)| aligned(Line::from(fit(text, *width)), *right)),
    )
    .style(theme::muted().add_modifier(Modifier::BOLD))
    .bottom_margin(1);
    let widget = Table::new(
        body(table, &widths, &right, ui, focused),
        widths.iter().copied().map(Constraint::Length),
    )
    .header(header)
    .block(block)
    .column_spacing(COLUMN_GAP)
    .highlight_spacing(HighlightSpacing::Always)
    .highlight_symbol(if focused {
        Text::styled(HIGHLIGHT, theme::accent())
    } else {
        Text::raw("  ")
    })
    .row_highlight_style(if focused {
        theme::selected().add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    });
    StatefulWidget::render(widget, area, buffer, &mut ui.rows);
    buffer.set_string(
        inner.x,
        inner.y + 1,
        "─".repeat(usize::from(inner.width)),
        theme::edge(false),
    );
    if table.rows.is_empty() {
        Paragraph::new(Line::styled(table.empty.as_str(), theme::muted()))
            .centered()
            .render(Rect::new(inner.x, inner.y + 2, inner.width, 1), buffer);
    }
    marks(buffer, inner, table, ui, focused, sink);
    scrollbar(
        buffer,
        area,
        inner,
        table.rows.len(),
        ui.rows.offset(),
        focused,
    );
    sink.table = Some(area);
    if focused {
        sink.focus = Some(area);
    }
}

fn body<'a>(
    table: &'a app::Table,
    widths: &'a [u16],
    right: &'a [bool],
    ui: &'a Ui,
    focused: bool,
) -> impl Iterator<Item = Row<'static>> + 'a {
    table.rows.iter().enumerate().map(move |(index, row)| {
        let cells = row
            .cells
            .iter()
            .zip(widths)
            .zip(right)
            .map(|((cell, width), right)| {
                let lead = lead(cell.tone);
                let mut spans = Vec::new();
                if lead > 0 {
                    spans.push(Span::styled(DOT, theme::tone(cell.tone)));
                }
                spans.push(Span::styled(
                    fit(&cell.text, width.saturating_sub(lead)),
                    written(cell.tone),
                ));
                aligned(Line::from(spans), *right)
            });
        let current = focused && ui.rows.selected() == Some(index);
        let style = if ui.hovered(&Hit::Row(index)) && !current {
            theme::raised()
        } else {
            Style::new()
        };
        Row::new(cells).style(style)
    })
}

fn marks(
    buffer: &mut Buffer,
    inner: Rect,
    table: &app::Table,
    ui: &Ui,
    focused: bool,
    sink: &mut Sink,
) {
    let offset = ui.rows.offset();
    for (line, row) in (offset..table.rows.len()).enumerate() {
        let y = inner.y + 2 + cells(line);
        if y >= inner.bottom() {
            break;
        }
        let current = focused && ui.rows.selected() == Some(row);
        if table.rows[row].warn && !current {
            buffer.set_string(inner.x, y, WARN, theme::level(Level::Warning));
        }
        let hit = Hit::Row(row);
        if ui.hovered(&hit)
            && let Some(first) = table.rows[row].cells.first()
            && table.rows[row].open.is_some()
        {
            sink.tooltip = Some(hints::open(&first.text));
        }
        sink.hits.push((Rect::new(inner.x, y, inner.width, 1), hit));
    }
}

fn scrollbar(
    buffer: &mut Buffer,
    area: Rect,
    inner: Rect,
    rows: usize,
    offset: usize,
    focused: bool,
) {
    let visible = usize::from(inner.height.saturating_sub(2));
    if rows <= visible {
        return;
    }
    let track = Rect::new(area.right() - 1, inner.y + 2, 1, inner.height - 2);
    let mut state = ScrollbarState::new(rows - visible)
        .position(offset)
        .viewport_content_length(visible);
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some(TRACK))
        .track_style(theme::edge(false))
        .thumb_symbol(THUMB)
        .thumb_style(if focused {
            theme::accent()
        } else {
            theme::muted()
        })
        .render(track, buffer, &mut state);
}

fn aligned(line: Line<'static>, right: bool) -> Cell<'static> {
    Cell::from(if right { line.right_aligned() } else { line })
}

fn widths(table: &app::Table, available: u16) -> Vec<u16> {
    let mut widths: Vec<u16> = table
        .columns
        .iter()
        .enumerate()
        .map(|(column, title)| {
            table
                .rows
                .iter()
                .filter_map(|row| row.cells.get(column))
                .map(|cell| wrap::width(&cell.text) + lead(cell.tone))
                .chain([wrap::width(title)])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let room = available.saturating_sub(COLUMN_GAP * cells(widths.len().saturating_sub(1)));
    while widths.iter().sum::<u16>() > room {
        match widths.iter_mut().max() {
            Some(widest) if *widest > MIN_COLUMN => *widest -= 1,
            _ => break,
        }
    }
    widths
}

fn fit(text: &str, width: u16) -> String {
    if wrap::width(text) <= width {
        return text.to_owned();
    }
    let mut fitted = String::with_capacity(text.len());
    let mut used = 0;
    for c in text.chars() {
        let size = cells(c.width().unwrap_or(0));
        if used + size + 1 > width {
            break;
        }
        used += size;
        fitted.push(c);
    }
    fitted.push('…');
    fitted
}

fn numeric(table: &app::Table, column: usize) -> bool {
    let mut cells = table
        .rows
        .iter()
        .filter_map(|row| row.cells.get(column))
        .map(|cell| cell.text.as_str())
        .filter(|text| !text.is_empty())
        .peekable();
    cells.peek().is_some()
        && cells.all(|text| text.starts_with(|c: char| c.is_ascii_digit() || "$€<+-−—".contains(c)))
}

fn marked(buffer: &mut Buffer, area: Rect, level: Level, text: &str) -> u16 {
    let style = match level {
        Level::Warning | Level::Error => theme::level(level),
        Level::Info | Level::Success => theme::soft(),
    };
    buffer.set_string(area.x, area.y, theme::icon(level), theme::level(level));
    let lines = wrap::lines(text, area.width.saturating_sub(2));
    for (row, line) in lines.iter().enumerate() {
        buffer.set_string(area.x + 2, area.y + cells(row), line, style);
    }
    cells(lines.len())
}

fn buttons(buffer: &mut Buffer, area: Rect, page: &Page, ui: &Ui, sizes: &Sizes, sink: &mut Sink) {
    for (index, (button, (column, row))) in page.buttons.iter().zip(&sizes.buttons).enumerate() {
        let (x, y) = (area.x + column, area.y + row);
        let text = button_text(&button.label);
        let hit = Hit::Button(index);
        let hovered = ui.hovered(&hit);
        let focused = ui.focus == Some(Target::Button(index));
        let pill = if focused {
            Pill::Focused
        } else if hovered {
            Pill::Hovered
        } else if button.action == Action::Submit {
            Pill::Primary
        } else {
            Pill::Idle
        };
        if hovered && let Some(key) = hints::shortcut(&button.action) {
            sink.tooltip = Some(hints::key_for(key));
        }
        let room = area.right().saturating_sub(x);
        buffer.set_stringn(x, y, &text, usize::from(room), theme::pill(pill));
        let rect = Rect::new(x, y, wrap::width(&text).min(room), 1);
        sink.hits.push((rect, hit));
        if focused {
            sink.focus = Some(rect);
        }
    }
}

fn button_text(label: &str) -> String {
    format!("  {label}  ")
}

fn secret(buffer: &mut Buffer, area: Rect, page: &Page, ui: &Ui) {
    let Some(secret) = &page.secret else {
        return;
    };
    let words: Vec<&str> = secret.words().collect();
    let (columns, rows) = grid(words.len(), area.width);
    let wanted = match words.as_slice() {
        [key] => cells(key.chars().count() + key.chars().count() / KEY_GROUP) + 3,
        _ => columns * WORD_COLUMN + 4,
    };
    let area = Rect {
        width: area.width.min(wanted),
        ..area
    };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::edge(false));
    if !ui.words_shown {
        block = block.title_bottom(Line::styled(
            format!(" {} ", hints::WORDS_HIDDEN),
            theme::soft(),
        ));
    }
    let inner = block.inner(area);
    block.render(area, buffer);
    if let [key] = words.as_slice() {
        let shown: Zeroizing<String> = if ui.words_shown {
            grouped(key)
        } else {
            grouped(&MASK.to_string().repeat(key.chars().count()))
        };
        let style = if ui.words_shown {
            theme::strong()
        } else {
            theme::muted()
        };
        buffer.set_stringn(
            inner.x + 1,
            inner.y,
            shown.as_str(),
            usize::from(inner.width.saturating_sub(1)),
            style,
        );
        return;
    }
    for (index, word) in words.iter().enumerate() {
        let column = cells(index) / rows;
        let row = cells(index) % rows;
        let x = inner.x + 1 + column * WORD_COLUMN;
        let y = inner.y + row;
        buffer.set_string(x, y, format!("{:>2}.", index + 1), theme::muted());
        let (shown, style) = if ui.words_shown {
            (*word, theme::strong())
        } else {
            (HIDDEN_WORD, theme::muted())
        };
        buffer.set_string(x + 4, y, shown, style);
    }
}

fn grouped(text: &str) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(text.len() * 2));
    for (index, c) in text.chars().enumerate() {
        if index > 0 && index % KEY_GROUP == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn grid(count: usize, width: u16) -> (u16, u16) {
    let columns = (width.saturating_sub(4) / WORD_COLUMN).clamp(1, 4);
    (columns, cells(count).div_ceil(columns))
}

fn input_inner(width: u16) -> u16 {
    width.min(INPUT_WIDTH).saturating_sub(4)
}

fn indent(group: &Group, column: u16) -> u16 {
    if group.label.is_empty() { 0 } else { column }
}

fn column<'a>(labels: impl Iterator<Item = &'a str>) -> u16 {
    labels.map(wrap::width).max().unwrap_or(0).min(MAX_LABEL) + 2
}

fn count(text: Option<&str>, width: u16) -> u16 {
    text.map_or(0, |text| cells(wrap::lines(text, width).len()))
}

fn is_toggle(group: &Group) -> bool {
    matches!(group.choices.as_slice(), [choice] if matches!(choice.action, Action::Toggle(_)))
}

fn choice_text(choice: &Choice, toggle: bool) -> String {
    match (toggle, choice.selected, choice.tone) {
        (true, true, _) => format!(" ✓ {} ", choice.label),
        (true, false, _) => format!(" ○ {} ", choice.label),
        (false, _, Tone::Brand(_)) => format!(" {DOT}{} ", choice.label),
        (false, _, _) => format!(" {} ", choice.label),
    }
}

fn choices_width(group: &Group) -> u16 {
    let toggle = is_toggle(group);
    group
        .choices
        .iter()
        .map(|choice| wrap::width(&choice_text(choice, toggle)) + CHOICE_GAP)
        .sum::<u16>()
        .saturating_sub(CHOICE_GAP)
}
