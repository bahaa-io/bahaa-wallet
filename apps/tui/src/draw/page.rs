use bahaa_app_core::{Page, Picture, View};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use super::parts::{self, Part, Sink, Sizes};
use super::picture;
use super::qr::{Code, Scale};
use crate::hints;
use crate::theme;
use crate::ui::Ui;

const MAX_WIDTH: u16 = 104;
const MIN_TABLE: u16 = 10;
const BESIDE_QR: u16 = 46;
const BESIDE_SCALE: Scale = Scale::Half;
const BESIDE_PICTURE: u16 = 32;

enum Art<'a> {
    Code(Code),
    Picture(&'a Picture, u16, u16),
}

impl Art<'_> {
    fn width(&self) -> u16 {
        match self {
            Self::Code(code) => code.width(BESIDE_SCALE),
            Self::Picture(_, width, _) => *width,
        }
    }

    fn height(&self) -> u16 {
        match self {
            Self::Code(code) => code.height(BESIDE_SCALE),
            Self::Picture(_, _, height) => *height,
        }
    }

    fn draw(&self, buffer: &mut Buffer, area: Rect) {
        match self {
            Self::Code(code) => code.draw(buffer, area, BESIDE_SCALE),
            Self::Picture(shown, ..) => {
                picture::draw(shown, buffer, area, theme::base().bg.unwrap_or_default());
            }
        }
    }
}

fn beside(page: &Page, area: Rect) -> Option<Art<'_>> {
    let room = area.width.saturating_sub(BESIDE_QR + 2);
    if let Some(code) = page.qr.as_deref().and_then(Code::new) {
        return code
            .fits(BESIDE_SCALE, room, area.height)
            .then_some(Art::Code(code));
    }
    let shown = page.picture.as_ref()?;
    let (width, height) = picture::fit(shown, room.min(BESIDE_PICTURE), area.height)?;
    Some(Art::Picture(shown, width, height))
}

struct Plan {
    parts: Vec<(Part, u16, u16)>,
    sizes: Sizes,
    total: u16,
}

impl Plan {
    fn new(page: &Page, ui: &Ui, width: u16, titled: bool, room: Option<u16>) -> Self {
        let sizes = Sizes::new(page, width);
        let list = parts::list(page, titled);
        let mut heights: Vec<u16> = list
            .iter()
            .map(|part| parts::height(*part, page, ui, &sizes))
            .collect();
        let gaps: Vec<u16> = list
            .iter()
            .enumerate()
            .map(|(index, part)| {
                let previous = index.checked_sub(1).map(|previous| list[previous]);
                u16::from(previous.is_some_and(|previous| !sizes.joined(page, previous, *part)))
            })
            .collect();
        if let (Some(room), Some(table)) = (room, list.iter().position(|part| *part == Part::Table))
        {
            let others: u16 =
                heights.iter().sum::<u16>() - heights[table] + gaps.iter().sum::<u16>();
            heights[table] = heights[table].min(
                room.saturating_sub(others)
                    .max(MIN_TABLE.min(heights[table])),
            );
        }
        let mut y = 0;
        let parts = list
            .into_iter()
            .zip(heights.into_iter().zip(gaps))
            .map(|(part, (height, gap))| {
                y += gap;
                let placed = (part, y, height);
                y += height;
                placed
            })
            .collect();
        Self {
            parts,
            sizes,
            total: y,
        }
    }
}

pub(crate) fn height(view: &View, ui: &Ui, width: u16) -> u16 {
    Plan::new(&view.page, ui, width.min(MAX_WIDTH), false, None).total
}

pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &View,
    ui: &mut Ui,
    titled: bool,
    fill: Style,
) {
    let page = &view.page;
    let beside = beside(page, area);
    let flow = Rect {
        width: beside
            .as_ref()
            .map_or(area.width, |art| area.width - art.width() - 2),
        ..area
    };
    let width = flow.width.min(MAX_WIDTH);
    let plan = Plan::new(page, ui, width, titled, Some(flow.height));
    let mut canvas = Buffer::empty(Rect::new(0, 0, width, plan.total.max(1)));
    canvas.set_style(canvas.area, fill);
    let mut sink = Sink::default();
    for (part, y, height) in &plan.parts {
        parts::draw(
            *part,
            Rect::new(0, *y, width, *height),
            &mut canvas,
            page,
            ui,
            &plan.sizes,
            &mut sink,
        );
    }
    ui.vertical.clone_from(&plan.sizes.vertical);
    if ui.follow
        && let Some(focus) = sink.focus
    {
        if focus.y < ui.scroll {
            ui.scroll = focus.y;
        } else if focus.bottom() > ui.scroll + flow.height {
            ui.scroll = focus.bottom() - flow.height;
        }
    }
    ui.scroll = ui.scroll.min(plan.total.saturating_sub(flow.height));
    let shown = |rect: Rect| {
        let moved = Rect {
            x: flow.x + rect.x,
            y: (flow.y + rect.y).checked_sub(ui.scroll)?,
            ..rect
        };
        let visible = moved.intersection(flow);
        (!visible.is_empty()).then_some(visible)
    };
    let hits: Vec<_> = sink
        .hits
        .into_iter()
        .filter_map(|(rect, hit)| Some((shown(rect)?, hit)))
        .collect();
    let table = sink.table.and_then(shown);
    let cursor = sink.cursor.and_then(|at| {
        shown(Rect::new(at.x, at.y, 1, 1)).map(|rect| Position::new(rect.x, rect.y))
    });
    let scroll = ui.scroll;
    ui.hits.extend(hits);
    ui.table = table;
    if sink.tooltip.is_some() {
        ui.tooltip = sink.tooltip;
    }
    let buffer = frame.buffer_mut();
    copy(&canvas, buffer, flow, scroll);
    let edge = flow.x + width.saturating_sub(1);
    if scroll > 0 {
        buffer.set_string(edge, flow.y, hints::MORE_ABOVE, theme::accent());
    }
    if plan.total > scroll + flow.height {
        buffer.set_string(edge, flow.bottom() - 1, hints::MORE_BELOW, theme::accent());
    }
    if let Some(art) = &beside {
        art.draw(
            buffer,
            Rect::new(
                area.right() - art.width(),
                area.y,
                art.width(),
                art.height(),
            ),
        );
    }
    if let Some(cursor) = cursor
        && ui.overlay.is_none()
        && ui.update.is_none()
    {
        frame.set_cursor_position(cursor);
    }
}

fn copy(canvas: &Buffer, screen: &mut Buffer, area: Rect, scroll: u16) {
    let rows = area.height.min(canvas.area.height.saturating_sub(scroll));
    let columns = area.width.min(canvas.area.width);
    for y in 0..rows {
        for x in 0..columns {
            screen[(area.x + x, area.y + y)] = canvas[(x, y + scroll)].clone();
        }
    }
}
