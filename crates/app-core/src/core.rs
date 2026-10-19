use std::sync::Arc;
use std::time::Duration;

use bahaa_engine::{
    Account, Activity, ActivityKind, Address, AssetId, Coin, Contact, CustomToken, Event, Fiat,
    Network, NetworkId, Portfolio, Price, Registry, SentStatus, SentTx, Settings, State, Status,
    WalletId, WalletInfo,
};

use crate::action::{Action, Go, Tab};
use crate::call::{Call, Load, Loaded, Outcome};
use crate::screen::{self, Screen};
use crate::view::{Choice, Level, Page, Request, Toast, View};
use crate::{format, text};

const TOAST_FOR: u64 = 5;
const CLIPBOARD_FOR: Duration = Duration::from_secs(30);
const GLOBAL: u64 = 0;

pub(crate) enum Input {
    Action(Action),
    State(State),
    Portfolio(Box<Portfolio>),
    Event(Box<Event>),
    Lagged,
    Done(u64, Box<Outcome>),
    Loaded(Loaded),
    Timer(Timer),
    Tick(u64),
}

pub(crate) enum Effect {
    Call(u64, Call),
    Load(Load),
    Request(Request),
    After(Duration, Timer),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Timer {
    ClearClipboard(u64),
}

#[derive(Default)]
pub(crate) struct Data {
    pub wallets: Vec<WalletInfo>,
    pub accounts: Vec<Account>,
    pub portfolio: Portfolio,
    pub activity: Vec<Activity>,
    pub contacts: Vec<Contact>,
    pub tokens: Vec<CustomToken>,
    pub settings: Settings,
}

pub(crate) struct Ctx<'a> {
    pub registry: &'a Registry,
    pub data: &'a Data,
    pub now: u64,
}

impl Ctx<'_> {
    pub(crate) fn wallet_name(&self, id: WalletId) -> String {
        let position = self.data.wallets.iter().position(|wallet| wallet.id == id);
        position
            .and_then(|position| self.data.wallets[position].name.clone())
            .unwrap_or_else(|| text::wallet(position.map_or(0, |position| position + 1)))
    }

    pub(crate) fn coin(&self, asset: &AssetId) -> Option<&Coin> {
        bahaa_engine::coin(self.registry, &self.data.tokens, asset)
    }

    pub(crate) fn symbol(&self, asset: &AssetId) -> String {
        self.coin(asset)
            .map_or_else(|| text::UNKNOWN.to_owned(), |coin| coin.symbol.clone())
    }

    pub(crate) fn native_symbol(&self, network: &NetworkId) -> String {
        self.symbol(&AssetId::Native(network.clone()))
    }

    pub(crate) fn network(&self, id: &NetworkId) -> Option<&Network> {
        self.registry.get(id)
    }

    pub(crate) fn network_name(&self, id: &NetworkId) -> String {
        self.network(id)
            .map_or_else(|| id.to_string(), |network| network.name.clone())
    }

    pub(crate) fn networks(&self) -> Vec<&Network> {
        let mut shown: Vec<&Network> = self
            .registry
            .networks()
            .filter(|network| !network.testnet || self.data.settings.show_testnets)
            .collect();
        shown.sort_by(|a, b| (a.testnet, &a.name).cmp(&(b.testnet, &b.name)));
        shown
    }

    pub(crate) fn shown(&self, address: &Address, network: &NetworkId) -> String {
        self.network(network).map_or_else(
            || address.to_string(),
            |network| format::address(address, network),
        )
    }

    pub(crate) fn party(&self, address: &Address, network: &NetworkId) -> String {
        self.contact(network, address).map_or_else(
            || format::short(&self.shown(address, network)),
            |contact| contact.name.clone(),
        )
    }

    pub(crate) fn contact(&self, network: &NetworkId, address: &Address) -> Option<&Contact> {
        self.data
            .contacts
            .iter()
            .find(|contact| contact.network == *network && contact.address == *address)
    }

    pub(crate) fn price(&self, asset: &AssetId) -> Option<Price> {
        self.data
            .portfolio
            .holdings
            .iter()
            .find(|holding| holding.asset == *asset)
            .and_then(|holding| holding.price)
    }

    pub(crate) fn fiat(&self) -> Fiat {
        self.data.settings.fiat
    }

    pub(crate) fn owner(&self, address: &Address) -> Option<WalletId> {
        self.data
            .accounts
            .iter()
            .find(|account| account.address == *address)
            .map(|account| account.wallet)
    }

    pub(crate) fn account_index(&self, wallet: WalletId, address: &Address) -> Option<u32> {
        self.data
            .accounts
            .iter()
            .find(|account| account.wallet == wallet && account.address == *address)
            .map(|account| account.index)
    }

    pub(crate) fn sent(&self, network: &NetworkId, id: &str) -> Option<&SentTx> {
        self.data
            .activity
            .iter()
            .find_map(|activity| match &activity.kind {
                ActivityKind::Sent(tx) if tx.network == *network && tx.id == id => Some(tx),
                _ => None,
            })
    }
}

#[derive(Default)]
pub(crate) struct Reply {
    pub nav: Option<Nav>,
    pub call: Option<Call>,
    pub request: Option<Request>,
    pub note: Option<(Level, String)>,
}

impl Reply {
    pub(crate) fn call(call: Call) -> Self {
        Self {
            call: Some(call),
            ..Self::default()
        }
    }

    pub(crate) fn nav(nav: Nav) -> Self {
        Self {
            nav: Some(nav),
            ..Self::default()
        }
    }

    pub(crate) fn request(request: Request) -> Self {
        Self {
            request: Some(request),
            ..Self::default()
        }
    }

    pub(crate) fn note(level: Level, text: impl Into<String>) -> Self {
        Self::default().with_note(level, text)
    }

    pub(crate) fn with_note(mut self, level: Level, text: impl Into<String>) -> Self {
        self.note = Some((level, text.into()));
        self
    }
}

pub(crate) enum Nav {
    Push(Box<dyn Screen>),
    Replace(Box<dyn Screen>),
    Back,
    Tab(Tab),
    Finish,
}

pub(crate) struct Core {
    registry: Arc<Registry>,
    now: u64,
    state: Option<State>,
    onboarding: bool,
    data: Data,
    tab: Tab,
    stack: Vec<Box<dyn Screen>>,
    toasts: Vec<(Toast, u64)>,
    counter: u64,
    pending: Option<u64>,
    clipboard: u64,
}

impl Core {
    pub(crate) fn new(registry: Arc<Registry>, now: u64) -> Self {
        Self {
            registry,
            now,
            state: None,
            onboarding: false,
            data: Data::default(),
            tab: Tab::Portfolio,
            stack: vec![screen::starting()],
            toasts: Vec::new(),
            counter: GLOBAL,
            pending: None,
            clipboard: 0,
        }
    }

    pub(crate) fn update(&mut self, input: Input) -> Vec<Effect> {
        match input {
            Input::Action(action) => self.act(&action),
            Input::State(state) => self.state(state),
            Input::Portfolio(portfolio) => {
                self.data.portfolio = *portfolio;
                Vec::new()
            }
            Input::Event(event) => self.event(*event),
            Input::Lagged => self.reload(),
            Input::Done(ticket, outcome) => self.done(ticket, *outcome),
            Input::Loaded(loaded) => {
                self.loaded(loaded);
                Vec::new()
            }
            Input::Timer(Timer::ClearClipboard(generation)) => {
                if generation == self.clipboard {
                    vec![Effect::Request(Request::ClearClipboard)]
                } else {
                    Vec::new()
                }
            }
            Input::Tick(now) => {
                self.now = now;
                self.toasts.retain(|(_, until)| *until > now);
                Vec::new()
            }
        }
    }

    pub(crate) fn view(&self) -> View {
        let ctx = self.ctx();
        View {
            page: self.top().map_or_else(Page::default, |top| top.page(&ctx)),
            tabs: if self.in_tabs() {
                Tab::ALL
                    .into_iter()
                    .map(|tab| Choice {
                        label: text::tab(tab).to_owned(),
                        action: Action::Go(Go::Tab(tab)),
                        selected: tab == self.tab,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            banner: self.banner(),
            toasts: self.toasts.iter().map(|(toast, _)| toast.clone()).collect(),
            busy: self.pending.is_some(),
        }
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            registry: &self.registry,
            data: &self.data,
            now: self.now,
        }
    }

    fn parts(&mut self) -> (Option<&mut Box<dyn Screen>>, Ctx<'_>) {
        (
            self.stack.last_mut(),
            Ctx {
                registry: &self.registry,
                data: &self.data,
                now: self.now,
            },
        )
    }

    fn top(&self) -> Option<&dyn Screen> {
        self.stack.last().map(AsRef::as_ref)
    }

    fn in_tabs(&self) -> bool {
        self.state == Some(State::Unlocked) && !self.onboarding
    }

    fn act(&mut self, action: &Action) -> Vec<Effect> {
        match action {
            Action::Dismiss(id) => {
                self.toasts.retain(|(toast, _)| toast.id != *id);
                Vec::new()
            }
            Action::Lock if self.state == Some(State::Unlocked) => {
                vec![Effect::Call(GLOBAL, Call::Lock)]
            }
            Action::Refresh if self.in_tabs() => vec![Effect::Call(GLOBAL, Call::Refresh)],
            Action::Quit => vec![Effect::Request(Request::Quit)],
            Action::Go(Go::Tab(tab)) if self.in_tabs() => {
                self.open_tab(*tab);
                Vec::new()
            }
            Action::Back => {
                let reply = self
                    .stack
                    .last_mut()
                    .and_then(|top| top.back())
                    .unwrap_or_else(|| Reply::nav(Nav::Back));
                self.apply(reply)
            }
            _ => {
                let (top, ctx) = self.parts();
                let reply = top.map_or_else(Reply::default, |top| top.act(action, &ctx));
                self.apply(reply)
            }
        }
    }

    fn done(&mut self, ticket: u64, outcome: Outcome) -> Vec<Effect> {
        if ticket == GLOBAL {
            return match outcome {
                Outcome::Done(Err(err)) => {
                    self.toast(Level::Error, text::error(&err));
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        if self.pending != Some(ticket) {
            return Vec::new();
        }
        self.pending = None;
        let (top, ctx) = self.parts();
        let reply = top.map_or_else(Reply::default, |top| top.done(outcome, &ctx));
        self.apply(reply)
    }

    fn apply(&mut self, reply: Reply) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some((level, text)) = reply.note {
            self.toast(level, text);
        }
        if let Some(request) = reply.request {
            if let Request::Copy(_) = request {
                self.clipboard += 1;
                effects.push(Effect::After(
                    CLIPBOARD_FOR,
                    Timer::ClearClipboard(self.clipboard),
                ));
                self.toast(Level::Info, text::COPIED.to_owned());
            }
            effects.push(Effect::Request(request));
        }
        match reply.nav {
            Some(Nav::Push(screen)) => self.stack.push(screen),
            Some(Nav::Replace(screen)) => {
                self.stack.pop();
                self.stack.push(screen);
            }
            Some(Nav::Back) if self.stack.len() > 1 => {
                self.stack.pop();
            }
            Some(Nav::Tab(tab)) => self.open_tab(tab),
            Some(Nav::Finish) => {
                self.onboarding = false;
                self.open_tab(Tab::Portfolio);
            }
            Some(Nav::Back) | None => {}
        }
        if let Some(call) = reply.call {
            self.counter += 1;
            self.pending = Some(self.counter);
            effects.push(Effect::Call(self.counter, call));
        }
        effects
    }

    fn open_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.pending = None;
        self.stack = vec![screen::tab(tab)];
    }

    fn state(&mut self, state: State) -> Vec<Effect> {
        if self.state.replace(state) == Some(state) {
            return Vec::new();
        }
        match state {
            State::NoVault => {
                self.clear();
                self.onboarding = true;
                self.stack = vec![screen::welcome(false)];
                Vec::new()
            }
            State::Locked => {
                self.clear();
                self.stack = vec![screen::unlock()];
                Vec::new()
            }
            State::Unlocked => {
                if !self.onboarding {
                    self.open_tab(Tab::Portfolio);
                }
                self.reload()
            }
        }
    }

    fn clear(&mut self) {
        self.data = Data::default();
        self.onboarding = false;
        self.pending = None;
    }

    fn reload(&self) -> Vec<Effect> {
        if self.state == Some(State::Unlocked) {
            Load::ALL.into_iter().map(Effect::Load).collect()
        } else {
            Vec::new()
        }
    }

    fn loaded(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::Wallets(wallets) => self.data.wallets = wallets,
            Loaded::Accounts(accounts) => self.data.accounts = accounts,
            Loaded::Activity(activity) => self.data.activity = activity,
            Loaded::Contacts(contacts) => self.data.contacts = contacts,
            Loaded::Tokens(tokens) => self.data.tokens = tokens,
            Loaded::Settings(settings) => self.data.settings = settings,
        }
    }

    fn event(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::StateChanged(_) => Vec::new(),
            Event::WalletsChanged(wallets) => {
                self.data.wallets = wallets;
                Vec::new()
            }
            Event::AccountsChanged(accounts) => {
                self.data.accounts = accounts;
                Vec::new()
            }
            Event::SettingsChanged(settings) => {
                self.data.settings = settings;
                Vec::new()
            }
            Event::ContactsChanged(contacts) => {
                self.data.contacts = contacts;
                Vec::new()
            }
            Event::TokensChanged(tokens) => {
                self.data.tokens = tokens;
                Vec::new()
            }
            Event::TxUpdated(tx) => {
                self.settled(&tx);
                vec![Effect::Load(Load::Activity)]
            }
            Event::Received(arrivals) => {
                for arrival in arrivals
                    .iter()
                    .filter(|arrival| arrival.cautions.is_empty())
                {
                    let received = &arrival.received;
                    let ctx = self.ctx();
                    let message = text::received(
                        &format::amount(received.amount, &ctx.symbol(&received.asset)),
                        &ctx.network_name(&received.network),
                    );
                    self.toast(Level::Success, message);
                }
                vec![Effect::Load(Load::Activity)]
            }
            Event::SentElsewhere(_) => vec![Effect::Load(Load::Activity)],
        }
    }

    fn settled(&mut self, tx: &SentTx) {
        let ctx = self.ctx();
        let before = ctx.sent(&tx.network, &tx.id).map(|known| &known.status);
        if tx.status.is_open() || before == Some(&tx.status) {
            return;
        }
        let amount = format::amount(tx.amount, &ctx.symbol(&tx.asset));
        let (level, message) = match &tx.status {
            SentStatus::Confirmed => (Level::Success, text::confirmed(&amount)),
            SentStatus::Failed(reason) => (Level::Error, text::failed(&amount, reason)),
            _ => (Level::Warning, text::dropped(&amount)),
        };
        self.toast(level, message);
    }

    fn toast(&mut self, level: Level, text: String) {
        self.counter += 1;
        self.toasts.push((
            Toast {
                id: self.counter,
                level,
                text,
            },
            self.now + TOAST_FOR,
        ));
    }

    fn banner(&self) -> Option<String> {
        if !self.in_tabs() {
            return None;
        }
        let ctx = self.ctx();
        let down: Vec<String> = self
            .data
            .portfolio
            .networks
            .iter()
            .filter(|health| health.problem.is_some())
            .map(|health| ctx.network_name(&health.network))
            .collect();
        if !down.is_empty() {
            return Some(text::unreachable(&down));
        }
        (self.data.portfolio.prices == Status::Offline).then(|| text::PRICES_OFFLINE.to_owned())
    }
}
