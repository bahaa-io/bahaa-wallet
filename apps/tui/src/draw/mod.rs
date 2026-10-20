mod overlay;
mod page;
mod parts;
mod qr;

use bahaa_app_core::{Action, Banner, Level, PageId, View};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget};

use crate::hints;
use crate::theme;
use crate::ui::{Hit, Overlay, Ui};
use crate::wrap::{self, cells};

const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;
const CARD_WIDTH: u16 = 76;
const TOAST_WIDTH: u16 = 48;
const TOASTS: usize = 3;
const LOGO_HEIGHT: u16 = 5;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn frame(frame: &mut Frame<'_>, view: &View, ui: &mut Ui) {
    ui.begin();
    paint(frame, view, ui);
    if ui.hover_moved() {
        frame.buffer_mut().reset();
        ui.begin();
        paint(frame, view, ui);
    }
}

fn paint(frame: &mut Frame<'_>, view: &View, ui: &mut Ui) {
    let area = frame.area();
    frame.buffer_mut().set_style(area, theme::base());
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        centered_text(frame.buffer_mut(), area, hints::TOO_SMALL, theme::text());
        return;
    }
    let [top, _, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    header(frame.buffer_mut(), top, view, ui);
    if view.page.id == PageId::Starting {
        logo(
            frame.buffer_mut(),
            body,
            body.y + body.height.saturating_sub(LOGO_HEIGHT) / 2,
        );
    } else if view.tabs.is_empty() {
        card(frame, body, view, ui);
    } else {
        let inner = Rect {
            x: body.x + 2,
            y: body.y,
            width: body.width.saturating_sub(4),
            height: body.height.saturating_sub(1),
        };
        page::render(frame, inner, view, ui, true, theme::base());
    }
    toasts(frame.buffer_mut(), body, view, ui);
    match &ui.overlay {
        Some(Overlay::Qr) => overlay::qr(frame.buffer_mut(), area, view),
        Some(Overlay::Keys) => overlay::keys(frame.buffer_mut(), area),
        Some(Overlay::Menu(_)) => overlay::menu(frame, area, view, ui),
        None => {}
    }
    let spinner = SPINNER[ui.spinner % SPINNER.len()];
    overlay::update(frame.buffer_mut(), area, ui, spinner);
    footer(frame.buffer_mut(), bottom, view, ui);
}

fn header(buffer: &mut Buffer, area: Rect, view: &View, ui: &mut Ui) {
    buffer.set_style(area, theme::panel());
    let tabs: Vec<String> = view
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| format!(" {} {} ", index + 1, tab.label))
        .collect();
    let tabs_width: u16 = tabs.iter().map(|tab| wrap::width(tab)).sum();
    let mut x = area.x + 1;
    if let Some(line) = brand(area.width.saturating_sub(tabs_width + 7)) {
        let width = cells(line.width());
        buffer.set_line(x, area.y, &line, width);
        x += width + 2;
    }
    for (label, tab) in tabs.iter().zip(&view.tabs) {
        let width = wrap::width(label);
        let hit = Hit::Action(tab.action.clone());
        let hovered = ui.hovered(&hit);
        let (number, name) = label.split_at(2);
        let line = if tab.selected {
            Line::styled(label.as_str(), theme::pill(theme::Pill::Focused))
        } else {
            let fill = if hovered {
                ui.tooltip = Some(hints::go_to(&tab.label));
                theme::raised()
            } else {
                Style::new()
            };
            Line::from(vec![
                Span::styled(number, theme::muted().patch(fill)),
                Span::styled(name, theme::text().patch(fill)),
            ])
        };
        buffer.set_line(x, area.y, &line, width);
        ui.hits.push((Rect::new(x, area.y, width, 1), hit));
        x += width;
    }
    if let Some(banner) = &view.banner {
        status(buffer, area, x + 2, banner, ui);
    }
}

fn brand(room: u16) -> Option<Line<'static>> {
    let full = format!("{} {}", hints::BRAND_NAME, hints::BRAND_WALLET);
    [full.as_str(), hints::BRAND_NAME, ""]
        .into_iter()
        .map(|name| {
            let mut spans = vec![Span::styled(hints::BRAND_MARK, theme::accent())];
            if !name.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(name.to_owned(), theme::strong()));
            }
            Line::from(spans)
        })
        .find(|line| cells(line.width()) + 2 <= room)
}

fn status(buffer: &mut Buffer, area: Rect, start: u16, banner: &Banner, ui: &mut Ui) {
    let room = area.right().saturating_sub(start);
    let icon = theme::icon(banner.level);
    let text = [format!(" {icon} {} ", banner.text), format!(" {icon} ")]
        .into_iter()
        .find(|text| wrap::width(text) <= room);
    let Some(text) = text else {
        return;
    };
    let width = wrap::width(&text);
    let hit = Hit::Action(banner.action.clone());
    let fill = if ui.hovered(&hit) {
        ui.tooltip = Some(hints::OPEN_STATUS.to_owned());
        theme::raised()
    } else {
        Style::new()
    };
    let (lead, rest) = text.split_at(1 + icon.len());
    let tone = match banner.level {
        Level::Success | Level::Info => theme::soft(),
        level => theme::level(level),
    };
    let line = Line::from(vec![
        Span::styled(lead, theme::level(banner.level).patch(fill)),
        Span::styled(rest, tone.patch(fill)),
    ]);
    let x = area.right() - width - 1;
    buffer.set_line(x, area.y, &line, width);
    ui.hits.push((Rect::new(x, area.y, width, 1), hit));
}

fn footer(buffer: &mut Buffer, area: Rect, view: &View, ui: &Ui) {
    buffer.set_style(area, theme::panel());
    let right = if let Some(notice) = ui.notice {
        Span::styled(
            format!("{} {notice} ", theme::icon(Level::Warning)),
            theme::level(Level::Warning),
        )
    } else if let Some(tooltip) = &ui.tooltip {
        Span::styled(format!("{tooltip} "), theme::soft())
    } else if view.busy {
        Span::styled(
            format!(
                "{} {} ",
                SPINNER[ui.spinner % SPINNER.len()],
                hints::WORKING
            ),
            theme::accent(),
        )
    } else {
        Span::raw("")
    };
    let right_width = wrap::width(&right.content);
    let room = area.width.saturating_sub(right_width + 2);
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for (key, label) in ui.hints(view) {
        let chip = format!(" {key} ");
        let text = format!(" {label}  ");
        let width = wrap::width(&chip) + wrap::width(&text);
        if used + width > room {
            break;
        }
        used += width;
        spans.push(Span::styled(chip, theme::key()));
        spans.push(Span::styled(text, theme::muted()));
    }
    buffer.set_line(area.x, area.y, &Line::from(spans), room);
    buffer.set_span(
        area.right().saturating_sub(right_width),
        area.y,
        &right,
        right_width,
    );
}

fn logo(buffer: &mut Buffer, area: Rect, y: u16) {
    let width = hints::LOGO
        .iter()
        .map(|line| wrap::width(line))
        .max()
        .unwrap_or(0);
    let x = area.x + area.width.saturating_sub(width) / 2;
    for (row, line) in hints::LOGO.iter().enumerate() {
        buffer.set_string(x, y + cells(row), line, theme::strong());
    }
    centered_text(
        buffer,
        Rect::new(area.x, y + 3, area.width, 1),
        hints::LOGO_WORD,
        theme::muted(),
    );
}

fn card(frame: &mut Frame<'_>, body: Rect, view: &View, ui: &mut Ui) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::edge(false))
        .title(Line::styled(
            format!(" {} ", view.page.title),
            theme::strong(),
        ))
        .padding(Padding::new(2, 2, 1, 1));
    let width = CARD_WIDTH.min(body.width.saturating_sub(4));
    let content = page::height(view, ui, width.saturating_sub(6));
    let height = (content + 4).min(body.height);
    let branded = body.height >= height + LOGO_HEIGHT + 2;
    let total = height + if branded { LOGO_HEIGHT } else { 0 };
    let top = body.y + (body.height - total) / 2;
    if branded {
        logo(frame.buffer_mut(), body, top);
    }
    let area = Rect {
        x: body.x + (body.width - width) / 2,
        y: top + if branded { LOGO_HEIGHT } else { 0 },
        width,
        height,
    };
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    page::render(frame, inner, view, ui, false, theme::base());
}

fn toasts(buffer: &mut Buffer, body: Rect, view: &View, ui: &mut Ui) {
    let mut bottom = body.bottom();
    for toast in view.toasts.iter().rev().take(TOASTS) {
        let lines = wrap::lines(&toast.text, TOAST_WIDTH - 6);
        let width = lines
            .iter()
            .map(|line| wrap::width(line))
            .max()
            .unwrap_or(0)
            + 6;
        let height = cells(lines.len()) + 2;
        if bottom < body.y + height {
            break;
        }
        let area = Rect::new(
            body.right().saturating_sub(width + 1),
            bottom - height,
            width,
            height,
        );
        let hit = Hit::Action(Action::Dismiss(toast.id));
        let hovered = ui.hovered(&hit);
        if hovered {
            ui.tooltip = Some(hints::DISMISS.to_owned());
        }
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::level(toast.level))
            .style(if hovered {
                theme::panel().patch(theme::raised())
            } else {
                theme::panel()
            })
            .padding(Padding::horizontal(1));
        let text: Vec<Line<'_>> = lines
            .into_iter()
            .enumerate()
            .map(|(row, line)| {
                let lead = if row == 0 {
                    Span::styled(
                        format!("{} ", theme::icon(toast.level)),
                        theme::level(toast.level),
                    )
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![lead, Span::styled(line, theme::text())])
            })
            .collect();
        Clear.render(area, buffer);
        Paragraph::new(text).block(block).render(area, buffer);
        ui.hits.push((area, hit));
        bottom -= height;
    }
}

fn centered_text(buffer: &mut Buffer, area: Rect, text: &str, style: Style) {
    let y = area.y + area.height / 2;
    Paragraph::new(Line::styled(text, style))
        .centered()
        .render(Rect::new(area.x, y, area.width, 1), buffer);
}
