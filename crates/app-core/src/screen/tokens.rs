use bahaa_engine::{AssetId, NetworkId};

use super::{Screen, asset_cell, button, cell, input, network_cell, network_group, picked_network};
use crate::action::{Action, Field, Go, Pick};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Level, Page, PageId, Row, Table, Tone};
use crate::{format, parse, text};

pub(crate) struct List;

impl Screen for List {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Go(Go::NewToken) => Reply::nav(Nav::Push(Box::new(Add::default()))),
            Action::Pick(Pick::RemoveToken(row)) => ctx
                .data
                .tokens
                .get(*row)
                .map_or_else(Reply::default, |token| {
                    Reply::call(Call::RemoveToken(token.asset.clone()))
                }),
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Ok(())) => Reply::note(Level::Success, text::TOKEN_REMOVED),
            Outcome::Done(Err(err)) => Reply::note(Level::Error, text::error(&err)),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let rows = ctx
            .data
            .tokens
            .iter()
            .map(|token| Row {
                cells: vec![
                    asset_cell(ctx, &token.asset),
                    token.coin.name.clone().into(),
                    network_cell(ctx, token.asset.network()),
                    cell(
                        token.asset.contract().map_or_else(String::new, |contract| {
                            format::short(&ctx.shown(contract, token.asset.network()))
                        }),
                        Tone::Muted,
                    ),
                ],
                open: None,
                warn: false,
            })
            .collect();
        let mut buttons = vec![button(text::ADD_TOKEN, Action::Go(Go::NewToken))];
        buttons.extend(ctx.data.tokens.iter().enumerate().map(|(position, token)| {
            button(
                text::remove_token(&token.coin.symbol),
                Action::Pick(Pick::RemoveToken(position)),
            )
        }));
        Page {
            id: PageId::Tokens,
            title: text::TOKENS.to_owned(),
            table: Some(Table {
                columns: text::TOKEN_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_TOKENS.to_owned(),
            }),
            buttons,
            ..Page::default()
        }
    }
}

#[derive(Default)]
struct Add {
    network: Option<NetworkId>,
    contract: String,
    error: Option<String>,
}

impl Screen for Add {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Network(position)) => {
                self.network = picked_network(ctx, *position);
            }
            Action::Type(Field::Contract, value) => self.contract.clone_from(value),
            Action::Submit => {
                let network = self
                    .network
                    .as_ref()
                    .and_then(|network| ctx.network(network));
                let Some(network) = network else {
                    self.error = Some(text::PICK_NETWORK.to_owned());
                    return Reply::default();
                };
                match parse::address(&self.contract, network.family()) {
                    Ok(contract) => {
                        self.error = None;
                        return Reply::call(Call::AddToken(AssetId::Token(
                            network.id.clone(),
                            contract,
                        )));
                    }
                    Err(problem) => self.error = Some(problem),
                }
            }
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Token(Ok(token)) => Reply::nav(Nav::Back)
                .with_note(Level::Success, text::token_added(&token.coin.symbol)),
            Outcome::Token(Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::NewToken,
            title: text::ADD_TOKEN.to_owned(),
            text: Some(text::ADD_TOKEN_TEXT.to_owned()),
            form: vec![
                network_group(ctx, self.network.as_ref()).into(),
                input(Field::Contract, text::CONTRACT, &self.contract).into(),
            ],
            error: self.error.clone(),
            buttons: vec![button(text::ADD, Action::Submit)],
            ..Page::default()
        }
    }
}
