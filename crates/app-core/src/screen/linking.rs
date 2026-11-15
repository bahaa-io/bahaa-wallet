use bahaa_engine::naming::{Link, NameRecord};
use bahaa_engine::{Address, EngineError, LinkDraft, LinkRequest, NetworkId, WalletId};

use super::{Screen, button, hidden, input, line, network_line, note};
use crate::action::{Action, Field};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Item, Level, Line, Note, Page, PageId, Secret};
use crate::{format, parse, text};

pub(crate) struct Target {
    pub wallet: WalletId,
    pub account: u32,
    pub record: NameRecord,
}

impl Target {
    pub(crate) fn request(&self, link: Link) -> LinkRequest {
        LinkRequest {
            wallet: self.wallet,
            account: self.account,
            network: self.record.network.clone(),
            name: self.record.name.clone(),
            link,
        }
    }

    fn network(&self) -> &NetworkId {
        &self.record.network
    }
}

pub(crate) struct LinkTo {
    target: Target,
    to: String,
}

impl LinkTo {
    pub(crate) fn new(target: Target) -> Self {
        Self {
            target,
            to: String::new(),
        }
    }

    fn address(&self, ctx: &Ctx<'_>) -> Option<Result<Address, String>> {
        let typed = self.to.trim();
        let family = ctx.network(self.target.network())?.family();
        (!typed.is_empty()).then(|| parse::address(typed, family))
    }
}

impl Screen for LinkTo {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::To, value) => {
                self.to.clone_from(value);
                Reply::default()
            }
            Action::Submit => match self.address(ctx) {
                Some(Ok(address)) => {
                    let request = self.target.request(Link::To(address));
                    Review::open(request, self.target.record.clone(), true)
                }
                _ => Reply::default(),
            },
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let hint = self.address(ctx).map(|parsed| match parsed {
            Ok(address) => note(
                Level::Success,
                match ctx.owner(&address) {
                    Some(wallet) => text::own_address(&ctx.owner_label(wallet, &address)),
                    None => text::valid_address(&ctx.network_name(self.target.network())),
                },
            ),
            Err(problem) => note(Level::Error, problem),
        });
        let mut to = input(Field::To, text::ADDRESS, &self.to);
        to.hint = hint;
        Page {
            id: PageId::LinkTo,
            title: text::link_title(&self.target.record.name),
            text: Some(text::LINK_TO_TEXT.to_owned()),
            form: vec![to.into()],
            buttons: vec![button(text::CONTINUE, Action::Submit)],
            ..Page::default()
        }
    }
}

enum Step {
    Checking,
    Ready(Box<LinkDraft>, u64),
    Confirm(Box<LinkDraft>, u64, Secret),
}

pub(crate) struct Review {
    request: LinkRequest,
    record: NameRecord,
    step: Step,
    error: Option<String>,
}

impl Review {
    pub(crate) fn open(request: LinkRequest, record: NameRecord, replace: bool) -> Reply {
        let call = Call::PrepareLink(request.clone());
        let screen = Box::new(Self {
            request,
            record,
            step: Step::Checking,
            error: None,
        });
        Reply {
            call: Some(call),
            ..Reply::nav(if replace {
                Nav::Replace(screen)
            } else {
                Nav::Push(screen)
            })
        }
    }

    fn title(&self) -> String {
        let name = &self.record.name;
        match self.request.link {
            Link::To(_) => text::link_title(name),
            Link::Clear => text::unlink_title(name),
            Link::Renew => text::renew_title(name),
            Link::Primary => text::primary_title(name),
        }
    }

    fn review_page(&self, draft: &LinkDraft, at: u64, ctx: &Ctx<'_>) -> Page {
        let network = &draft.network;
        let native = ctx.native_symbol(network);
        let pointed = |to: Option<&Address>| {
            to.map_or_else(|| text::NOTHING.to_owned(), |to| ctx.party(to, network))
        };
        let after = match draft.link {
            Link::To(to) => Some(to),
            Link::Clear => None,
            Link::Renew | Link::Primary => draft.record.wallet,
        };
        let mut lines: Vec<Line> = vec![
            line(text::NAME, draft.record.name.clone()),
            line(text::POINTS_NOW, pointed(draft.record.wallet.as_ref())),
        ];
        if !matches!(draft.link, Link::Renew | Link::Primary) {
            lines.push(line(text::WILL_POINT, pointed(after.as_ref())));
        }
        lines.push(line(text::FROM, ctx.party(&draft.from, network)));
        lines.push(network_line(ctx, network));
        lines.push(line(
            text::FEE,
            text::with_value(
                &format::exact(draft.fee.expected(), &native),
                &format::fiat(
                    super::native_worth(ctx, network, draft.fee.expected()),
                    ctx.fiat(),
                ),
            ),
        ));
        let mut notes: Vec<Note> = Vec::new();
        if let Some(attach) = draft.attach {
            let attach = format::exact(attach, &native);
            lines.push(line(text::ATTACHED, attach.clone()));
            notes.push(note(
                Level::Info,
                if draft.returned {
                    text::returned_note(&attach)
                } else {
                    text::stays_note(&attach)
                },
            ));
        }
        if draft.record.renewable {
            notes.push(note(
                Level::Info,
                text::renews_note(&draft.record.name, &format::when(at + YEAR)),
            ));
        }
        if super::expired(at, draft.valid_for, ctx.now) {
            notes.push(note(Level::Warning, text::REVIEW_OLD));
        }
        Page {
            id: PageId::LinkReview,
            title: self.title(),
            lines,
            notes,
            error: self.error.clone(),
            buttons: vec![button(
                if super::expired(at, draft.valid_for, ctx.now) {
                    text::CHECK_AGAIN
                } else {
                    text::CONTINUE
                },
                Action::Submit,
            )],
            ..Page::default()
        }
    }

    fn check_again(&mut self) -> Reply {
        self.step = Step::Checking;
        Reply::call(Call::PrepareLink(self.request.clone()))
    }
}

const YEAR: u64 = 365 * 24 * 3600;

impl Screen for Review {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match (&mut self.step, action) {
            (Step::Confirm(_, _, password), Action::Type(Field::Password, value)) => {
                *password = Secret::new(value);
                Reply::default()
            }
            (Step::Checking, Action::Submit) if self.error.is_some() => {
                self.error = None;
                self.check_again()
            }
            (Step::Ready(draft, at), Action::Submit) => {
                if super::expired(*at, draft.valid_for, ctx.now) {
                    return self.check_again();
                }
                self.step = Step::Confirm(draft.clone(), *at, Secret::default());
                Reply::default()
            }
            (Step::Confirm(draft, _, password), Action::Submit) => {
                Reply::call(Call::ConfirmLink(draft.id, super::secret(password)))
            }
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::LinkDrafted(Ok(draft)) => {
                self.error = None;
                self.step = Step::Ready(Box::new(draft), ctx.now);
            }
            Outcome::LinkDrafted(Err(err)) => {
                self.step = Step::Checking;
                self.error = Some(text::error(&err));
            }
            Outcome::Sent(Ok(_)) => {
                return Reply::nav(Nav::Back)
                    .with_note(Level::Success, text::link_sent(&self.record.name));
            }
            Outcome::Sent(Err(EngineError::DraftExpired | EngineError::UnknownDraft)) => {
                return self.check_again();
            }
            Outcome::Sent(Err(err)) => {
                if let Step::Confirm(_, _, password) = &mut self.step {
                    *password = Secret::default();
                }
                self.error = Some(text::error(&err));
            }
            _ => {}
        }
        Reply::default()
    }

    fn back(&mut self) -> Option<Reply> {
        let Step::Confirm(draft, at, _) = &self.step else {
            return None;
        };
        self.step = Step::Ready(draft.clone(), *at);
        self.error = None;
        Some(Reply::default())
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        match &self.step {
            Step::Checking => {
                let mut page = Page {
                    id: PageId::LinkReview,
                    title: self.title(),
                    text: Some(text::CHECKING_NAME.to_owned()),
                    error: self.error.clone(),
                    ..Page::default()
                };
                if self.error.is_some() {
                    page.buttons.push(button(text::CHECK_AGAIN, Action::Submit));
                }
                page
            }
            Step::Ready(draft, at) => self.review_page(draft, *at, ctx),
            Step::Confirm(_, _, password) => Page {
                id: PageId::Approve,
                title: self.title(),
                form: vec![Item::from(hidden(
                    Field::Password,
                    text::PASSWORD,
                    password,
                ))],
                error: self.error.clone(),
                buttons: vec![button(text::SEND_NOW, Action::Submit)],
                ..Page::default()
            },
        }
    }
}
