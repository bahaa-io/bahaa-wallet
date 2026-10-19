use zeroize::Zeroizing;

use super::{Screen, button, hidden, kept};
use crate::action::{Action, Field};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Reply};
use crate::text;
use crate::view::{Page, PageId};

#[derive(Default)]
pub(crate) struct Unlock {
    password: Zeroizing<String>,
    error: Option<String>,
}

impl Screen for Unlock {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Password, value) => self.password = kept(value),
            Action::Submit if self.password.is_empty() => {
                self.error = Some(text::ENTER_PASSWORD.to_owned());
            }
            Action::Submit => {
                self.error = None;
                return Reply::call(Call::Unlock(super::secret(&self.password)));
            }
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        if let Outcome::Done(Err(err)) = outcome {
            self.password = Zeroizing::default();
            self.error = Some(text::error(&err));
        }
        Reply::default()
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::Unlock,
            title: text::UNLOCK.to_owned(),
            inputs: vec![hidden(Field::Password, text::PASSWORD)],
            error: self.error.clone(),
            buttons: vec![button(text::UNLOCK_BUTTON, Action::Submit)],
            ..Page::default()
        }
    }
}
