use bahaa_engine::{Contact, NetworkId};

use super::{
    Screen, button, cell, input, line, network_cell, network_group, network_line, picked_network,
};
use crate::action::{Action, Field, Go, Pick};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Level, Page, PageId, Row, Table, Tone};
use crate::{format, parse, text};

pub(crate) struct List;

impl Screen for List {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Go(Go::Contact(row)) => ctx
                .data
                .contacts
                .get(*row)
                .map_or_else(Reply::default, |contact| {
                    Reply::nav(Nav::Push(Box::new(Edit::existing(contact.clone()))))
                }),
            Action::Go(Go::NewContact) => Reply::nav(Nav::Push(Box::new(Edit::new()))),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let rows = ctx
            .data
            .contacts
            .iter()
            .enumerate()
            .map(|(position, contact)| Row {
                cells: vec![
                    contact.name.clone().into(),
                    network_cell(ctx, &contact.network),
                    cell(
                        format::short(&ctx.shown(&contact.address, &contact.network)),
                        Tone::Muted,
                    ),
                ],
                open: Some(Action::Go(Go::Contact(position))),
                warn: false,
            })
            .collect();
        Page {
            id: PageId::Contacts,
            title: text::CONTACTS.to_owned(),
            table: Some(Table {
                columns: text::CONTACT_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_CONTACTS.to_owned(),
            }),
            buttons: vec![button(text::ADD_CONTACT, Action::Go(Go::NewContact))],
            ..Page::default()
        }
    }
}

struct Edit {
    existing: Option<Contact>,
    name: String,
    network: Option<NetworkId>,
    address: String,
    deleting: bool,
    error: Option<String>,
}

impl Edit {
    fn new() -> Self {
        Self {
            existing: None,
            name: String::new(),
            network: None,
            address: String::new(),
            deleting: false,
            error: None,
        }
    }

    fn existing(contact: Contact) -> Self {
        Self {
            name: contact.name.clone(),
            existing: Some(contact),
            ..Self::new()
        }
    }

    fn contact(&self, ctx: &Ctx<'_>) -> Result<Contact, String> {
        let name = self.name.trim();
        if let Some(existing) = &self.existing {
            return Ok(Contact {
                name: name.to_owned(),
                ..existing.clone()
            });
        }
        let network = self.network.as_ref().ok_or(text::PICK_NETWORK)?;
        let family = ctx.network(network).ok_or(text::PICK_NETWORK)?.family();
        Ok(Contact {
            network: network.clone(),
            address: parse::address(&self.address, family)?,
            name: name.to_owned(),
        })
    }
}

impl Screen for Edit {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Name, value) => self.name.clone_from(value),
            Action::Type(Field::Address, value) if self.existing.is_none() => {
                self.address.clone_from(value);
            }
            Action::Pick(Pick::Network(position)) if self.existing.is_none() => {
                self.network = picked_network(ctx, *position);
            }
            Action::Pick(Pick::Delete) => {
                if let Some(contact) = &self.existing {
                    self.deleting = true;
                    return Reply::call(Call::RemoveContact(
                        contact.network.clone(),
                        contact.address,
                    ));
                }
            }
            Action::Submit => match self.contact(ctx) {
                Ok(contact) => {
                    self.deleting = false;
                    return Reply::call(Call::SaveContact(contact));
                }
                Err(problem) => self.error = Some(problem),
            },
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Ok(())) => Reply::nav(Nav::Back).with_note(
                Level::Success,
                if self.deleting {
                    text::CONTACT_REMOVED
                } else {
                    text::CONTACT_SAVED
                },
            ),
            Outcome::Done(Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let mut page = Page {
            id: PageId::Contact,
            title: if self.existing.is_some() {
                text::EDIT_CONTACT
            } else {
                text::NEW_CONTACT
            }
            .to_owned(),
            form: vec![input(Field::Name, text::NAME, &self.name).into()],
            error: self.error.clone(),
            buttons: vec![button(text::SAVE, Action::Submit)],
            ..Page::default()
        };
        if let Some(contact) = &self.existing {
            page.lines = vec![
                network_line(ctx, &contact.network),
                line(text::ADDRESS, ctx.shown(&contact.address, &contact.network)),
            ];
            page.buttons
                .push(button(text::DELETE, Action::Pick(Pick::Delete)));
        } else {
            page.form.extend([
                network_group(ctx, self.network.as_ref()).into(),
                input(Field::Address, text::ADDRESS, &self.address).into(),
            ]);
        }
        page
    }
}
