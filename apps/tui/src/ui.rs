use std::collections::{HashMap, HashSet};

use bahaa_app_core::{Action, Field, Group, Item, Page, PageId, Value, View, completions};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;
use semver::Version;

use crate::editor::Editor;
use crate::hints::{self, Hint};
use crate::palette::{Command, Palette};
use crate::update::{Done, Release};

const PAGE_STEP: usize = 10;
const WHEEL: u16 = 3;
const SCROLL: u16 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Group(usize),
    Input(Field),
    Table,
    Button(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    Action(Action),
    Focus(Target),
    Choice(usize, usize),
    Row(usize),
    Button(usize),
    Entry(usize),
    Update(bool),
}

pub(crate) enum Prompt {
    Offer(Release),
    Installing(Version),
    Installed(Version),
    Latest,
    Failed(&'static str),
}

pub(crate) enum Overlay {
    Qr,
    Keys,
    Menu(Palette),
}

#[derive(Default)]
pub(crate) struct Ui {
    pub(crate) focus: Option<Target>,
    pub(crate) editors: HashMap<Field, Editor>,
    pub(crate) cursors: Vec<usize>,
    pub(crate) vertical: Vec<bool>,
    pub(crate) rows: TableState,
    pub(crate) scroll: u16,
    pub(crate) follow: bool,
    pub(crate) words_shown: bool,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) update: Option<Prompt>,
    pub(crate) install: Option<Release>,
    pub(crate) notes_scroll: u16,
    pub(crate) hits: Vec<(Rect, Hit)>,
    pub(crate) table: Option<Rect>,
    pub(crate) notice: Option<&'static str>,
    pub(crate) tooltip: Option<String>,
    pub(crate) spinner: usize,
    hover: Option<Hit>,
    pointer: Option<Position>,
    page: Option<PageId>,
    toggled: HashSet<Field>,
    sent: u64,
}

impl Ui {
    pub(crate) fn sent(&mut self, number: u64) {
        self.sent = number;
    }

    pub(crate) fn spin(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    pub(crate) fn notice(&mut self, result: Result<(), &'static str>) {
        self.notice = result.err();
    }

    pub(crate) fn begin(&mut self) {
        self.hover = self.pointer.and_then(|at| self.hit_at(at));
        self.hits.clear();
        self.table = None;
        self.tooltip = None;
    }

    pub(crate) fn updates(&mut self, done: Done) {
        let prompt = match done {
            Done::Checked {
                result: Ok(Some(release)),
                ..
            } => Prompt::Offer(release),
            Done::Checked {
                asked: true,
                result: Ok(None),
            } => Prompt::Latest,
            Done::Checked {
                asked: true,
                result: Err(problem),
            }
            | Done::Installed(Err(problem)) => Prompt::Failed(problem),
            Done::Checked { asked: false, .. } => return,
            Done::Installed(Ok(version)) => Prompt::Installed(version),
        };
        self.notes_scroll = 0;
        self.update = Some(prompt);
    }

    pub(crate) fn installing(&self) -> bool {
        matches!(self.update, Some(Prompt::Installing(_)))
    }

    fn prompt_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return vec![Action::Quit];
        }
        match (&self.update, key.code) {
            (Some(Prompt::Installing(_)) | None, _) => {}
            (Some(Prompt::Offer(_)), KeyCode::Enter | KeyCode::Char('y')) => self.confirm_update(),
            (Some(Prompt::Offer(_)), KeyCode::Up | KeyCode::Char('k')) => {
                self.notes_scroll = self.notes_scroll.saturating_sub(1);
            }
            (Some(Prompt::Offer(_)), KeyCode::Down | KeyCode::Char('j')) => {
                self.notes_scroll = self.notes_scroll.saturating_add(1);
            }
            (Some(Prompt::Offer(_)), KeyCode::PageUp) => {
                self.notes_scroll = self.notes_scroll.saturating_sub(SCROLL);
            }
            (Some(Prompt::Offer(_)), KeyCode::PageDown) => {
                self.notes_scroll = self.notes_scroll.saturating_add(SCROLL);
            }
            (Some(_), KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n' | 'q')) => {
                self.update = None;
            }
            (Some(_), _) => {}
        }
        Vec::new()
    }

    fn confirm_update(&mut self) {
        if let Some(Prompt::Offer(release)) = self.update.take() {
            self.update = Some(Prompt::Installing(release.version.clone()));
            self.install = Some(release);
        }
    }

    fn prompt_click(&mut self, hit: Option<&Hit>) -> Vec<Action> {
        match hit {
            Some(Hit::Update(true)) => self.confirm_update(),
            Some(Hit::Update(false)) if !self.installing() => self.update = None,
            _ => {}
        }
        Vec::new()
    }

    pub(crate) fn hover_moved(&self) -> bool {
        self.pointer.and_then(|at| self.hit_at(at)) != self.hover
    }

    pub(crate) fn hovered(&self, hit: &Hit) -> bool {
        self.hover.as_ref() == Some(hit)
    }

    pub(crate) fn masked(&self, field: Field) -> bool {
        let hidden_by_default = !matches!(field, Field::Phrase | Field::Word(_));
        hidden_by_default != self.toggled.contains(&field)
    }

    pub(crate) fn suggestions(&self, field: Field) -> Vec<&'static str> {
        let Some(word) = self.word(field) else {
            return Vec::new();
        };
        let found = completions(word);
        if found == [word] { Vec::new() } else { found }
    }

    fn completion(&self, field: Field) -> Option<&'static str> {
        let word = self.word(field)?;
        let found = completions(word);
        if found.contains(&word) {
            None
        } else {
            found.first().copied()
        }
    }

    fn word(&self, field: Field) -> Option<&str> {
        if self.focus != Some(Target::Input(field))
            || !matches!(field, Field::Phrase | Field::Word(_))
        {
            return None;
        }
        self.editors.get(&field).and_then(Editor::word)
    }

    fn hit_at(&self, at: Position) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, hit)| hit.clone())
    }

    pub(crate) fn sync(&mut self, view: &View) {
        let page = &view.page;
        if self.page != Some(page.id) {
            *self = Self {
                page: Some(page.id),
                focus: first(page),
                follow: true,
                notice: self.notice,
                spinner: self.spinner,
                pointer: self.pointer,
                sent: self.sent,
                update: self.update.take(),
                install: self.install.take(),
                notes_scroll: self.notes_scroll,
                ..Self::default()
            };
        }
        self.cursors.resize(page.form.len(), usize::MAX);
        for (cursor, item) in self.cursors.iter_mut().zip(&page.form) {
            if let Item::Group(group) = item
                && *cursor >= group.choices.len()
            {
                *cursor = group
                    .choices
                    .iter()
                    .position(|choice| choice.selected)
                    .unwrap_or(0);
            }
        }
        let adopt = view.handled >= self.sent;
        for input in page.inputs() {
            let value = input.value.text();
            let editor = self
                .editors
                .entry(input.field)
                .or_insert_with(|| Editor::new(value));
            if adopt && editor.text() != value {
                *editor = Editor::new(value);
            }
        }
        if adopt {
            self.editors
                .retain(|field, _| page.inputs().any(|input| input.field == *field));
        }
        let rows = page.table.as_ref().map_or(0, |table| table.rows.len());
        let row = self
            .rows
            .selected()
            .unwrap_or(0)
            .min(rows.saturating_sub(1));
        self.rows.select((rows > 0).then_some(row));
        if !self
            .focus
            .is_some_and(|focus| targets(page).contains(&focus))
        {
            self.focus = first(page);
        }
    }

    pub(crate) fn event(&mut self, event: &Event, view: &View) -> Vec<Action> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.notice = None;
                self.follow = true;
                self.key(*key, view)
            }
            Event::Mouse(mouse) => self.mouse(*mouse, view),
            Event::Paste(text) => self.paste(text),
            _ => Vec::new(),
        }
    }

    pub(crate) fn hints(&self, view: &View) -> Vec<Hint> {
        let page = &view.page;
        match self.update {
            Some(Prompt::Offer(_)) => return vec![hints::UPDATE, hints::NOT_NOW, hints::SCROLL],
            Some(Prompt::Installing(_)) => return Vec::new(),
            Some(_) => return vec![hints::OK],
            None => {}
        }
        match self.overlay {
            Some(Overlay::Menu(_)) => return vec![hints::MOVE, hints::RUN, hints::CLOSE],
            Some(Overlay::Qr | Overlay::Keys) => return vec![hints::CLOSE],
            None => {}
        }
        let typing = matches!(self.focus, Some(Target::Input(_)));
        let mut list = match self.focus {
            Some(Target::Input(field)) => {
                let mut list = vec![hints::SUBMIT];
                list.push(if self.completion(field).is_some() {
                    hints::COMPLETE
                } else {
                    hints::NEXT
                });
                if page
                    .inputs()
                    .any(|input| input.field == field && matches!(input.value, Value::Hidden(_)))
                {
                    list.push(hints::SHOW);
                }
                list
            }
            Some(Target::Group(group)) if self.vertical.get(group) == Some(&true) => {
                vec![hints::MOVE, hints::PICK, hints::NEXT]
            }
            Some(Target::Group(_)) => vec![hints::CHOOSE, hints::PICK, hints::NEXT],
            Some(Target::Table) => vec![hints::MOVE, hints::OPEN, hints::NEXT],
            Some(Target::Button(_)) => vec![hints::PRESS, hints::NEXT],
            None => Vec::new(),
        };
        if !typing {
            if page.secret.is_some() {
                list.push(hints::WORDS);
            }
            if page.qr.is_some() {
                list.push(hints::QR);
            }
            let has = |action: Action| page.buttons.iter().any(|button| button.action == action);
            if has(Action::Copy) {
                list.push(hints::COPY);
            }
            if has(Action::Explore) {
                list.push(hints::EXPLORE);
            }
        }
        list.push(hints::BACK);
        list.push(hints::MENU);
        if !typing {
            list.push(hints::KEYS);
        }
        if !view.tabs.is_empty() {
            if !typing {
                list.push(hints::TABS);
            }
            list.push(hints::LOCK);
        }
        list.push(if typing {
            hints::FORCE_QUIT
        } else {
            hints::QUIT
        });
        list
    }

    fn key(&mut self, key: KeyEvent, view: &View) -> Vec<Action> {
        if self.update.is_some() {
            return self.prompt_key(key);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match &self.overlay {
            Some(Overlay::Menu(_)) => return self.menu_key(key, view),
            Some(Overlay::Qr | Overlay::Keys) => {
                if key.code == KeyCode::Char('c') && control {
                    return vec![Action::Quit];
                }
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('v' | '?' | 'q') | KeyCode::F(1)
                ) {
                    self.overlay = None;
                }
                return Vec::new();
            }
            None => {}
        }
        match key.code {
            KeyCode::Char('c') if control => vec![Action::Quit],
            KeyCode::Char('l') if control => vec![Action::Lock],
            KeyCode::Char('k') if control => self.open_menu(),
            KeyCode::F(1) => self.open_keys(),
            KeyCode::Esc => vec![Action::Back],
            KeyCode::Tab => self
                .complete()
                .unwrap_or_else(|| self.moved(&view.page, true)),
            KeyCode::BackTab => self.moved(&view.page, false),
            _ => match self.focus {
                Some(Target::Input(field)) => self.typing(field, key, view),
                Some(Target::Group(group)) => self.choosing(group, key, view),
                Some(Target::Table) => self.browsing(key, view),
                Some(Target::Button(button)) => self.pressing(button, key, view),
                None => self.command(key, view),
            },
        }
    }

    fn menu_key(&mut self, key: KeyEvent, view: &View) -> Vec<Action> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::Menu(palette)) = &mut self.overlay else {
            return Vec::new();
        };
        let changed = match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                return Vec::new();
            }
            KeyCode::Char('k') if control => {
                self.overlay = None;
                return Vec::new();
            }
            KeyCode::Char('c') if control => return vec![Action::Quit],
            KeyCode::Enter => {
                return palette
                    .chosen(view)
                    .map(|command| self.run(command))
                    .unwrap_or_default();
            }
            KeyCode::Up => {
                palette.step(view, false);
                false
            }
            KeyCode::Down | KeyCode::Tab => {
                palette.step(view, true);
                false
            }
            KeyCode::Char('u') if control => palette.query.clear(),
            KeyCode::Char('w') if control => palette.query.delete_word(),
            KeyCode::Char(c) if !control => palette.query.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace => palette.query.backspace(),
            KeyCode::Left => {
                palette.query.left();
                false
            }
            KeyCode::Right => {
                palette.query.right();
                false
            }
            _ => false,
        };
        if changed {
            palette.selected = 0;
        }
        Vec::new()
    }

    fn open_menu(&mut self) -> Vec<Action> {
        self.overlay = Some(Overlay::Menu(Palette::default()));
        Vec::new()
    }

    fn open_keys(&mut self) -> Vec<Action> {
        self.overlay = Some(Overlay::Keys);
        Vec::new()
    }

    fn run(&mut self, command: Command) -> Vec<Action> {
        self.overlay = None;
        match command {
            Command::Act(action) => vec![action],
            Command::Qr => {
                self.overlay = Some(Overlay::Qr);
                Vec::new()
            }
            Command::Words => {
                self.words_shown = !self.words_shown;
                Vec::new()
            }
            Command::Keys => self.open_keys(),
        }
    }

    fn typing(&mut self, field: Field, key: KeyEvent, view: &View) -> Vec<Action> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return vec![Action::Submit],
            KeyCode::Up => return self.moved(&view.page, false),
            KeyCode::Down => return self.moved(&view.page, true),
            KeyCode::PageUp => return self.scrolled(false, SCROLL),
            KeyCode::PageDown => return self.scrolled(true, SCROLL),
            KeyCode::Char(digit) if key.modifiers.contains(KeyModifiers::ALT) => {
                return tab(digit, view);
            }
            KeyCode::Char('r') if control => {
                if !self.toggled.remove(&field) {
                    self.toggled.insert(field);
                }
                return Vec::new();
            }
            _ => {}
        }
        let Some(editor) = self.editors.get_mut(&field) else {
            return Vec::new();
        };
        let changed = match key.code {
            KeyCode::Char('u') if control => editor.clear(),
            KeyCode::Char('w') if control => editor.delete_word(),
            KeyCode::Char(c) if !control => editor.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::Left => {
                editor.left();
                false
            }
            KeyCode::Right => {
                editor.right();
                false
            }
            KeyCode::Home => {
                editor.home();
                false
            }
            KeyCode::End => {
                editor.end();
                false
            }
            _ => false,
        };
        if changed {
            vec![Action::Type(field, editor.text().to_owned())]
        } else {
            Vec::new()
        }
    }

    fn choosing(&mut self, group: usize, key: KeyEvent, view: &View) -> Vec<Action> {
        let Some(Item::Group(Group { choices, .. })) = view.page.form.get(group) else {
            return Vec::new();
        };
        let vertical = self.vertical.get(group) == Some(&true);
        let cursor = self.cursors.get(group).copied().unwrap_or(0);
        let moved = match key.code {
            KeyCode::Left if !vertical => cursor.saturating_sub(1),
            KeyCode::Right if !vertical => (cursor + 1).min(choices.len().saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') if vertical && cursor > 0 => cursor - 1,
            KeyCode::Down | KeyCode::Char('j') if vertical && cursor + 1 < choices.len() => {
                cursor + 1
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                return choices
                    .get(cursor)
                    .map(|choice| vec![choice.action.clone()])
                    .unwrap_or_default();
            }
            _ => return self.command(key, view),
        };
        if let Some(slot) = self.cursors.get_mut(group) {
            *slot = moved;
        }
        Vec::new()
    }

    fn browsing(&mut self, key: KeyEvent, view: &View) -> Vec<Action> {
        let rows = view.page.table.as_ref().map_or(0, |table| table.rows.len());
        let last = rows.saturating_sub(1);
        let row = self.rows.selected().unwrap_or(0);
        let moved = match key.code {
            KeyCode::Up | KeyCode::Char('k') if row > 0 => row - 1,
            KeyCode::Down | KeyCode::Char('j') if row < last => row + 1,
            KeyCode::PageUp => row.saturating_sub(PAGE_STEP),
            KeyCode::PageDown => (row + PAGE_STEP).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            KeyCode::Enter => return open(view, row),
            _ => return self.command(key, view),
        };
        self.rows.select(Some(moved));
        Vec::new()
    }

    fn pressing(&mut self, button: usize, key: KeyEvent, view: &View) -> Vec<Action> {
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => press(view, button),
            KeyCode::Left => self.moved(&view.page, false),
            KeyCode::Right => self.moved(&view.page, true),
            _ => self.command(key, view),
        }
    }

    fn command(&mut self, key: KeyEvent, view: &View) -> Vec<Action> {
        match key.code {
            KeyCode::Char('q') => vec![Action::Quit],
            KeyCode::Char('y') => vec![Action::Copy],
            KeyCode::Char('o') => vec![Action::Explore],
            KeyCode::Char('r') => vec![Action::Refresh],
            KeyCode::Char(':') => self.open_menu(),
            KeyCode::Char('?') => self.open_keys(),
            KeyCode::Char('s') if view.page.secret.is_some() => self.run(Command::Words),
            KeyCode::Char('v') if view.page.qr.is_some() => self.run(Command::Qr),
            KeyCode::Char(digit) if digit.is_ascii_digit() => tab(digit, view),
            KeyCode::Up | KeyCode::Char('k') => self.moved(&view.page, false),
            KeyCode::Down | KeyCode::Char('j') => self.moved(&view.page, true),
            KeyCode::PageUp => self.scrolled(false, SCROLL),
            KeyCode::PageDown => self.scrolled(true, SCROLL),
            _ => Vec::new(),
        }
    }

    fn scrolled(&mut self, down: bool, rows: u16) -> Vec<Action> {
        self.follow = false;
        self.scroll = if down {
            self.scroll.saturating_add(rows)
        } else {
            self.scroll.saturating_sub(rows)
        };
        Vec::new()
    }

    fn complete(&mut self) -> Option<Vec<Action>> {
        let Some(Target::Input(field)) = self.focus else {
            return None;
        };
        let word = self.completion(field)?;
        let editor = self.editors.get_mut(&field)?;
        editor.complete(word, field == Field::Phrase);
        Some(vec![Action::Type(field, editor.text().to_owned())])
    }

    fn moved(&mut self, page: &Page, forward: bool) -> Vec<Action> {
        let targets = targets(page);
        if !targets.is_empty() {
            let current = self
                .focus
                .and_then(|focus| targets.iter().position(|target| *target == focus));
            let next = match (current, forward) {
                (None, _) => 0,
                (Some(index), true) => (index + 1) % targets.len(),
                (Some(index), false) => (index + targets.len() - 1) % targets.len(),
            };
            self.focus = Some(targets[next]);
            self.follow = true;
        }
        Vec::new()
    }

    fn mouse(&mut self, mouse: MouseEvent, view: &View) -> Vec<Action> {
        let at = Position::new(mouse.column, mouse.row);
        self.pointer = Some(at);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if self.update.is_some() => {
                let hit = self.hit_at(at);
                self.prompt_click(hit.as_ref())
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if self.update.is_some() => {
                self.notes_scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    self.notes_scroll.saturating_sub(1)
                } else {
                    self.notes_scroll.saturating_add(1)
                };
                Vec::new()
            }
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_)
                if self.update.is_some() =>
            {
                Vec::new()
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.hit_at(at);
                if self.overlay.is_some() {
                    if let Some(Hit::Entry(index)) = hit {
                        return self.pick_entry(index, view);
                    }
                    self.overlay = None;
                    return Vec::new();
                }
                hit.map_or_else(Vec::new, |hit| self.click(hit, view))
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if self.overlay.is_some() {
                    self.overlay = None;
                    Vec::new()
                } else {
                    self.open_menu()
                }
            }
            MouseEventKind::ScrollUp => self.wheel(at, false, view),
            MouseEventKind::ScrollDown => self.wheel(at, true, view),
            _ => Vec::new(),
        }
    }

    fn pick_entry(&mut self, index: usize, view: &View) -> Vec<Action> {
        let Some(Overlay::Menu(palette)) = &mut self.overlay else {
            return Vec::new();
        };
        palette.selected = index;
        palette
            .chosen(view)
            .map(|command| self.run(command))
            .unwrap_or_default()
    }

    fn click(&mut self, hit: Hit, view: &View) -> Vec<Action> {
        self.notice = None;
        match hit {
            Hit::Action(action) => vec![action],
            Hit::Focus(target) => {
                self.focus = Some(target);
                Vec::new()
            }
            Hit::Choice(group, index) => {
                self.focus = Some(Target::Group(group));
                if let Some(cursor) = self.cursors.get_mut(group) {
                    *cursor = index;
                }
                match view.page.form.get(group) {
                    Some(Item::Group(group)) => group
                        .choices
                        .get(index)
                        .map(|choice| vec![choice.action.clone()])
                        .unwrap_or_default(),
                    _ => Vec::new(),
                }
            }
            Hit::Row(row) => {
                self.focus = Some(Target::Table);
                self.rows.select(Some(row));
                open(view, row)
            }
            Hit::Button(button) => {
                self.focus = Some(Target::Button(button));
                press(view, button)
            }
            Hit::Entry(_) | Hit::Update(_) => Vec::new(),
        }
    }

    fn wheel(&mut self, at: Position, down: bool, view: &View) -> Vec<Action> {
        if let Some(Overlay::Menu(palette)) = &mut self.overlay {
            palette.step(view, down);
            return Vec::new();
        }
        if self.table.is_some_and(|area| area.contains(at)) {
            let last = view
                .page
                .table
                .as_ref()
                .map_or(0, |table| table.rows.len().saturating_sub(1));
            let row = self.rows.selected().unwrap_or(0);
            self.rows.select(Some(if down {
                (row + 1).min(last)
            } else {
                row.saturating_sub(1)
            }));
            Vec::new()
        } else {
            self.scrolled(down, WHEEL)
        }
    }

    fn paste(&mut self, text: &str) -> Vec<Action> {
        if let Some(Overlay::Menu(palette)) = &mut self.overlay {
            if palette.query.insert(text) {
                palette.selected = 0;
            }
            return Vec::new();
        }
        let Some(Target::Input(field)) = self.focus else {
            return Vec::new();
        };
        let Some(editor) = self.editors.get_mut(&field) else {
            return Vec::new();
        };
        if editor.insert(text) {
            vec![Action::Type(field, editor.text().to_owned())]
        } else {
            Vec::new()
        }
    }
}

fn tab(digit: char, view: &View) -> Vec<Action> {
    digit
        .to_digit(10)
        .and_then(|number| usize::try_from(number).ok()?.checked_sub(1))
        .and_then(|index| view.tabs.get(index))
        .map(|tab| vec![tab.action.clone()])
        .unwrap_or_default()
}

fn targets(page: &Page) -> Vec<Target> {
    page.form
        .iter()
        .enumerate()
        .map(|(index, item)| match item {
            Item::Group(_) => Target::Group(index),
            Item::Input(input) => Target::Input(input.field),
        })
        .chain(has_rows(page).then_some(Target::Table))
        .chain((0..page.buttons.len()).map(Target::Button))
        .collect()
}

fn first(page: &Page) -> Option<Target> {
    page.inputs()
        .next()
        .map(|input| Target::Input(input.field))
        .or_else(|| has_rows(page).then_some(Target::Table))
        .or_else(|| (!page.buttons.is_empty()).then_some(Target::Button(0)))
        .or_else(|| targets(page).first().copied())
}

fn has_rows(page: &Page) -> bool {
    page.table
        .as_ref()
        .is_some_and(|table| !table.rows.is_empty())
}

fn open(view: &View, row: usize) -> Vec<Action> {
    view.page
        .table
        .as_ref()
        .and_then(|table| table.rows.get(row))
        .and_then(|row| row.open.clone())
        .into_iter()
        .collect()
}

fn press(view: &View, button: usize) -> Vec<Action> {
    view.page
        .buttons
        .get(button)
        .map(|button| vec![button.action.clone()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use bahaa_app_core::View;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::{Prompt, Ui};
    use crate::update::Done;
    use crate::update::tests::release;

    fn press(ui: &mut Ui, code: KeyCode) {
        let event = Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(ui.event(&event, &View::default()), Vec::new());
    }

    fn offer(ui: &mut Ui) {
        ui.updates(Done::Checked {
            asked: false,
            result: Ok(Some(release("0.5.0", &["- better sending"]))),
        });
        assert!(matches!(ui.update, Some(Prompt::Offer(_))));
    }

    #[test]
    fn an_update_installs_only_after_it_is_confirmed() {
        let mut ui = Ui::default();
        ui.updates(Done::Checked {
            asked: false,
            result: Ok(None),
        });
        ui.updates(Done::Checked {
            asked: false,
            result: Err("offline"),
        });
        assert!(ui.update.is_none());
        ui.updates(Done::Checked {
            asked: true,
            result: Ok(None),
        });
        assert!(matches!(ui.update, Some(Prompt::Latest)));
        offer(&mut ui);
        press(&mut ui, KeyCode::Esc);
        assert!(ui.update.is_none() && ui.install.is_none());
        offer(&mut ui);
        press(&mut ui, KeyCode::Enter);
        assert!(ui.install.take().is_some());
        press(&mut ui, KeyCode::Esc);
        assert!(ui.installing());
        ui.updates(Done::Installed(Ok(release("0.5.0", &[]).version)));
        assert!(matches!(ui.update, Some(Prompt::Installed(_))));
    }
}
