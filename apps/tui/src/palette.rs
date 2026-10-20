use bahaa_app_core::{Action, View};

use crate::editor::Editor;
use crate::hints;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Act(Action),
    Qr,
    Words,
    Keys,
}

pub(crate) struct Entry {
    pub(crate) label: String,
    pub(crate) kind: &'static str,
    pub(crate) key: Option<&'static str>,
    pub(crate) command: Command,
    pub(crate) marks: Vec<usize>,
}

#[derive(Default)]
pub(crate) struct Palette {
    pub(crate) query: Editor,
    pub(crate) selected: usize,
}

impl Palette {
    pub(crate) fn entries(&self, view: &View) -> Vec<Entry> {
        let query = self.query.text();
        let mut found: Vec<(bool, usize, Entry)> = all(view)
            .into_iter()
            .filter_map(|mut entry| {
                let (marks, joined) = matches(query, &entry.label)?;
                let first = marks.first().copied().unwrap_or(0);
                entry.marks = marks;
                Some((!joined, first, entry))
            })
            .collect();
        found.sort_by_key(|(scattered, first, _)| (*scattered, *first));
        found.into_iter().map(|(.., entry)| entry).collect()
    }

    pub(crate) fn chosen(&self, view: &View) -> Option<Command> {
        self.entries(view)
            .into_iter()
            .nth(self.selected)
            .map(|entry| entry.command)
    }

    pub(crate) fn step(&mut self, view: &View, down: bool) {
        let last = self.entries(view).len().saturating_sub(1);
        self.selected = if down {
            (self.selected + 1).min(last)
        } else {
            self.selected.saturating_sub(1)
        };
    }
}

fn all(view: &View) -> Vec<Entry> {
    let mut list = Vec::new();
    page_entries(view, &mut list);
    app_entries(view, &mut list);
    for entry in &mut list {
        entry.key = key(view, &entry.command);
    }
    list
}

fn page_entries(view: &View, list: &mut Vec<Entry>) {
    let page = &view.page;
    for button in &page.buttons {
        add(
            list,
            button.label.clone(),
            hints::KIND_ACTION,
            Command::Act(button.action.clone()),
        );
    }
    for group in page.groups() {
        for choice in &group.choices {
            let label = if group.label.is_empty() {
                choice.label.clone()
            } else {
                format!("{}: {}", group.label, choice.label)
            };
            add(
                list,
                label,
                hints::KIND_OPTION,
                Command::Act(choice.action.clone()),
            );
        }
    }
    for row in page.table.iter().flat_map(|table| &table.rows) {
        if let Some(open) = &row.open {
            let label = row
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .filter(|text| !text.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(" · ");
            add(list, label, hints::KIND_OPEN, Command::Act(open.clone()));
        }
    }
    if page.qr.is_some() {
        add(
            list,
            hints::SHOW_QR.to_owned(),
            hints::KIND_APP,
            Command::Qr,
        );
    }
    if page.secret.is_some() {
        add(
            list,
            hints::SHOW_WORDS.to_owned(),
            hints::KIND_APP,
            Command::Words,
        );
    }
}

fn app_entries(view: &View, list: &mut Vec<Entry>) {
    for tab in &view.tabs {
        add(
            list,
            hints::go_to(&tab.label),
            hints::KIND_TAB,
            Command::Act(tab.action.clone()),
        );
    }
    if let Some(banner) = &view.banner {
        add(
            list,
            hints::STATUS.to_owned(),
            hints::KIND_APP,
            Command::Act(banner.action.clone()),
        );
    }
    if !view.tabs.is_empty() {
        add(
            list,
            hints::REFRESH.to_owned(),
            hints::KIND_APP,
            Command::Act(Action::Refresh),
        );
        add(
            list,
            hints::LOCK_WALLET.to_owned(),
            hints::KIND_APP,
            Command::Act(Action::Lock),
        );
    }
    let app = [
        (hints::GO_BACK, Command::Act(Action::Back)),
        (hints::SHOW_KEYS, Command::Keys),
        (hints::QUIT_APP, Command::Act(Action::Quit)),
    ];
    for (label, command) in app {
        add(list, label.to_owned(), hints::KIND_APP, command);
    }
}

fn add(list: &mut Vec<Entry>, label: String, kind: &'static str, command: Command) {
    if !list.iter().any(|entry| entry.command == command) {
        list.push(Entry {
            label,
            kind,
            key: None,
            command,
            marks: Vec::new(),
        });
    }
}

fn key(view: &View, command: &Command) -> Option<&'static str> {
    match command {
        Command::Act(action) => view
            .tabs
            .iter()
            .position(|tab| tab.action == *action)
            .and_then(|index| hints::TAB_KEYS.get(index).copied())
            .or_else(|| hints::shortcut(action)),
        Command::Qr => Some("v"),
        Command::Words => Some("s"),
        Command::Keys => Some("?"),
    }
}

fn matches(query: &str, label: &str) -> Option<(Vec<usize>, bool)> {
    let fold = |text: &str| -> Vec<char> {
        text.chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect()
    };
    let lower = fold(label);
    let phrase = fold(query.trim());
    if phrase.is_empty() {
        return Some((Vec::new(), true));
    }
    if let Some(start) = lower
        .windows(phrase.len())
        .position(|window| window == phrase.as_slice())
    {
        return Some(((start..start + phrase.len()).collect(), true));
    }
    let mut marks = Vec::with_capacity(phrase.len());
    let mut next = 0;
    for c in phrase.into_iter().filter(|c| !c.is_whitespace()) {
        let at = lower.get(next..)?.iter().position(|other| *other == c)? + next;
        marks.push(at);
        next = at + 1;
    }
    Some((marks, false))
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn queries_match_whole_words_before_scattered_letters() {
        assert_eq!(
            matches("lock", "Lock the wallet"),
            Some((vec![0, 1, 2, 3], true))
        );
        assert_eq!(
            matches("wal", "Lock the wallet"),
            Some((vec![9, 10, 11], true))
        );
        assert_eq!(
            matches("ltw", "Lock the wallet"),
            Some((vec![0, 5, 9], false))
        );
        assert_eq!(matches("xyz", "Lock the wallet"), None);
        assert_eq!(matches("", "Lock"), Some((vec![], true)));
    }
}
