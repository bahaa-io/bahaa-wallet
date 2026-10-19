use bahaa_engine::Status;

use super::{Screen, button, cell, network_cell, note, toned_line};
use crate::action::Action;
use crate::core::{Ctx, Reply};
use crate::view::{Level, Page, PageId, Row, Table, Tone};
use crate::{format, text};

pub(crate) struct Networks;

impl Screen for Networks {
    fn act(&mut self, _action: &Action, _ctx: &Ctx<'_>) -> Reply {
        Reply::default()
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let portfolio = &ctx.data.portfolio;
        let mut notes = Vec::new();
        let rows = ctx
            .networks()
            .into_iter()
            .map(|network| {
                let health = portfolio
                    .networks
                    .iter()
                    .find(|health| health.network == network.id);
                let balances = match health {
                    None => cell(text::NOT_CHECKED, Tone::Waiting),
                    Some(health) if health.problem.is_some() => cell(text::UNREACHABLE, Tone::Bad),
                    Some(_) => cell(text::REACHABLE, Tone::Good),
                };
                let updated = health
                    .and_then(|health| health.updated_at)
                    .map_or_else(|| text::NO_VALUE.to_owned(), |at| format::ago(at, ctx.now));
                let history = match health {
                    None => cell(text::NO_VALUE, Tone::Muted),
                    Some(health) if health.history_problem.is_some() => {
                        cell(text::HISTORY_FAILED, Tone::Warning)
                    }
                    Some(_) => cell(text::HISTORY_OK, Tone::Good),
                };
                if let Some(health) = health {
                    if let Some(problem) = &health.problem {
                        notes.push(note(
                            Level::Warning,
                            text::network_problem(&network.name, problem),
                        ));
                    }
                    if let Some(problem) = &health.history_problem {
                        notes.push(note(
                            Level::Info,
                            text::history_problem(&network.name, problem),
                        ));
                    }
                }
                Row {
                    cells: vec![
                        network_cell(ctx, &network.id),
                        balances,
                        cell(updated, Tone::Muted),
                        history,
                    ],
                    open: None,
                    warn: false,
                }
            })
            .collect();
        let prices = match portfolio.prices {
            Status::Live | Status::Polling => Tone::Good,
            Status::Idle => Tone::Muted,
            Status::Offline => Tone::Bad,
        };
        Page {
            id: PageId::Networks,
            title: text::NETWORK_STATUS.to_owned(),
            text: Some(text::NETWORK_STATUS_TEXT.to_owned()),
            lines: vec![toned_line(
                text::PRICES,
                text::prices(portfolio.prices),
                prices,
            )],
            table: Some(Table {
                columns: text::NETWORK_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::CHECKING.to_owned(),
            }),
            notes,
            buttons: vec![button(text::REFRESH_NOW, Action::Refresh)],
            ..Page::default()
        }
    }
}
