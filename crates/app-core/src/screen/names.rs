use bahaa_engine::naming::NameRecord;

use super::linking::{self, mine};
use super::{Screen, button, input, line, network_line, note, who};
use crate::action::{Action, Field, Pick};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::text;
use crate::view::{Level, Page, PageId};

#[derive(Default)]
pub(crate) struct Names {
    typed: String,
    looking: bool,
    error: Option<String>,
}

impl Screen for Names {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Name, value) => {
                self.typed.clone_from(value);
                self.error = None;
                Reply::default()
            }
            Action::Submit if !self.typed.trim().is_empty() && !self.looking => {
                self.looking = true;
                self.error = None;
                Reply::call(Call::FindName(self.typed.trim().to_owned()))
            }
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        let Outcome::Named(found) = outcome else {
            return Reply::default();
        };
        self.looking = false;
        match found {
            Ok(record) => Name::open(record),
            Err(err) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
        }
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::Names,
            title: text::YOUR_NAMES.to_owned(),
            text: Some(
                if self.looking {
                    text::FINDING_NAME
                } else {
                    text::NAMES_TEXT
                }
                .to_owned(),
            ),
            form: vec![input(Field::Name, text::NAME, &self.typed).into()],
            error: self.error.clone(),
            buttons: vec![button(text::LOOK_UP, Action::Submit)],
            ..Page::default()
        }
    }
}

struct Name {
    record: NameRecord,
    primary: bool,
}

impl Name {
    fn open(record: NameRecord) -> Reply {
        let call = record
            .wallet
            .filter(|_| record.reversible)
            .map(|to| Call::NameOf(record.network.clone(), to));
        Reply {
            call,
            ..Reply::nav(Nav::Push(Box::new(Self {
                record,
                primary: false,
            })))
        }
    }
}

impl Screen for Name {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Link(choice)) => linking::choose(ctx, &self.record, *choice),
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        if let Outcome::NameOf(address, found) = outcome
            && Some(address) == self.record.wallet
        {
            self.primary = found.is_some_and(|found| found.name == self.record.name);
        }
        Reply::default()
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let record = &self.record;
        let network = &record.network;
        let mut lines = vec![
            network_line(ctx, network),
            line(text::HELD_BY, who(ctx, &record.owner, network)),
        ];
        lines.extend(linking::record_lines(ctx, record, self.primary));
        let mut notes = linking::record_notes(record);
        let buttons = linking::record_buttons(ctx, record, self.primary);
        if mine(ctx, &record.owner).is_none() {
            notes.push(note(Level::Info, text::NOT_HELD));
        }
        Page {
            id: PageId::Name,
            title: record.name.clone(),
            lines,
            notes,
            buttons,
            ..Page::default()
        }
    }
}
