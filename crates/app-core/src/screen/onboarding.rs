use bahaa_engine::{ChainFamily, KeyringError, NewPhrase, PhraseKind, SecretString};

use super::{Screen, button, choice, group, hidden, note, password_inputs, password_problem};
use crate::action::{Action, Family, Field, Kind, Pick, Words};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::text;
use crate::view::{Input, Level, Note, Page, PageId, Secret};

const CHECKED_WORDS: usize = 3;

pub(crate) struct Welcome {
    adding: bool,
}

impl Welcome {
    pub(crate) fn new(adding: bool) -> Self {
        Self { adding }
    }
}

impl Screen for Welcome {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Create(words)) if self.adding => {
                Reply::call(Call::Generate(new_phrase(*words)))
            }
            Action::Pick(Pick::Create(words)) => {
                Reply::nav(Nav::Push(Box::new(NewPassword::new(Next::Create(*words)))))
            }
            Action::Pick(Pick::Import) => Reply::nav(Nav::Push(Box::new(Import::new(self.adding)))),
            Action::Pick(Pick::ImportKey) => {
                Reply::nav(Nav::Push(Box::new(ImportKey::new(self.adding))))
            }
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Generated(Ok((_, phrase))) => {
                Reply::nav(Nav::Push(Box::new(ShowPhrase::new(&phrase))))
            }
            Outcome::Generated(Err(err)) => Reply::note(Level::Error, text::error(&err)),
            _ => Reply::default(),
        }
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::Welcome,
            title: if self.adding {
                text::ADD_WALLET
            } else {
                text::WELCOME
            }
            .to_owned(),
            text: Some(text::WELCOME_TEXT.to_owned()),
            buttons: vec![
                button(text::CREATE_12, Action::Pick(Pick::Create(Words::Twelve))),
                button(
                    text::CREATE_24,
                    Action::Pick(Pick::Create(Words::TwentyFour)),
                ),
                button(text::IMPORT, Action::Pick(Pick::Import)),
                button(text::IMPORT_KEY, Action::Pick(Pick::ImportKey)),
            ],
            ..Page::default()
        }
    }
}

enum Next {
    Create(Words),
    Import(Secret, Secret, Option<PhraseKind>),
    ImportKey(Secret, Family),
}

struct NewPassword {
    next: Next,
    password: Secret,
    confirm: Secret,
    created: bool,
    error: Option<String>,
}

impl NewPassword {
    fn new(next: Next) -> Self {
        Self {
            next,
            password: Secret::default(),
            confirm: Secret::default(),
            created: false,
            error: None,
        }
    }

    fn continue_call(&self) -> Call {
        match &self.next {
            Next::Create(words) => Call::Generate(new_phrase(*words)),
            Next::Import(phrase, passphrase, kind) => {
                Call::Import(super::secret(phrase), super::secret(passphrase), *kind)
            }
            Next::ImportKey(key, family) => Call::ImportKey(super::secret(key), chain(*family)),
        }
    }
}

impl Screen for NewPassword {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::NewPassword, value) => self.password = Secret::new(value),
            Action::Type(Field::Confirm, value) => self.confirm = Secret::new(value),
            Action::Submit if self.created => return Reply::call(self.continue_call()),
            Action::Submit => {
                if let Some(problem) = password_problem(&self.password, &self.confirm) {
                    self.error = Some(problem.to_owned());
                } else {
                    self.error = None;
                    return Reply::call(Call::Create(super::secret(&self.password)));
                }
            }
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Ok(())) => {
                self.created = true;
                Reply::call(self.continue_call())
            }
            Outcome::Generated(Ok((_, phrase))) => {
                Reply::nav(Nav::Replace(Box::new(ShowPhrase::new(&phrase))))
            }
            Outcome::Imported(Ok(_)) => {
                Reply::nav(Nav::Finish).with_note(Level::Success, text::IMPORTED)
            }
            Outcome::Done(Err(err))
            | Outcome::Generated(Err(err))
            | Outcome::Imported(Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::NewPassword,
            title: text::SET_PASSWORD.to_owned(),
            text: Some(text::SET_PASSWORD_TEXT.to_owned()),
            form: password_inputs(&self.password, &self.confirm),
            error: self.error.clone(),
            buttons: vec![button(text::CONTINUE, Action::Submit)],
            ..Page::default()
        }
    }
}

struct ShowPhrase {
    phrase: Secret,
}

impl ShowPhrase {
    fn new(phrase: &SecretString) -> Self {
        Self {
            phrase: super::revealed(phrase),
        }
    }
}

impl Screen for ShowPhrase {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Submit => Reply::nav(Nav::Push(Box::new(Verify::new(self.phrase.clone())))),
            _ => Reply::default(),
        }
    }

    fn back(&mut self) -> Option<Reply> {
        Some(Reply::note(Level::Info, text::PHRASE_FIRST))
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::Phrase,
            title: text::YOUR_PHRASE.to_owned(),
            text: Some(text::YOUR_PHRASE_TEXT.to_owned()),
            secret: Some(self.phrase.clone()),
            buttons: vec![button(text::WROTE_IT_DOWN, Action::Submit)],
            ..Page::default()
        }
    }
}

struct Verify {
    phrase: Secret,
    positions: [usize; CHECKED_WORDS],
    answers: [Secret; CHECKED_WORDS],
    error: Option<String>,
}

impl Verify {
    fn new(phrase: Secret) -> Self {
        let positions = pick_positions(phrase.words().count());
        Self {
            phrase,
            positions,
            answers: Default::default(),
            error: None,
        }
    }
}

impl Screen for Verify {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Word(slot), value) if *slot < CHECKED_WORDS => {
                self.answers[*slot] = Secret::new(value);
            }
            Action::Submit => {
                let words: Vec<&str> = self.phrase.words().collect();
                let wrong = self
                    .positions
                    .iter()
                    .zip(&self.answers)
                    .find(|(position, answer)| {
                        !answer
                            .expose()
                            .trim()
                            .eq_ignore_ascii_case(words[**position])
                    });
                match wrong {
                    Some((position, _)) => self.error = Some(text::word_mismatch(position + 1)),
                    None => {
                        return Reply::nav(Nav::Finish).with_note(Level::Success, text::BACKED_UP);
                    }
                }
            }
            _ => {}
        }
        Reply::default()
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::Verify,
            title: text::CHECK_PHRASE.to_owned(),
            text: Some(text::CHECK_PHRASE_TEXT.to_owned()),
            form: self
                .positions
                .iter()
                .enumerate()
                .map(|(slot, position)| {
                    hidden(
                        Field::Word(slot),
                        &text::word_number(position + 1),
                        &self.answers[slot],
                    )
                    .into()
                })
                .collect(),
            error: self.error.clone(),
            buttons: vec![button(text::CHECK, Action::Submit)],
            ..Page::default()
        }
    }
}

struct Import {
    adding: bool,
    phrase: Secret,
    passphrase: Secret,
    kind: Option<Kind>,
    check: Result<PhraseKind, KeyringError>,
    error: Option<String>,
}

impl Import {
    fn new(adding: bool) -> Self {
        Self {
            adding,
            phrase: Secret::default(),
            passphrase: Secret::default(),
            kind: None,
            check: Err(KeyringError::Mnemonic(
                bahaa_engine::MnemonicError::WordCount(0),
            )),
            error: None,
        }
    }

    fn recheck(&mut self) {
        self.check = bahaa_engine::check_phrase(self.phrase.expose(), self.passphrase.expose());
        self.error = None;
    }

    fn chosen(&self) -> Result<Option<PhraseKind>, String> {
        match (&self.check, self.kind) {
            (Ok(_), _) => Ok(None),
            (Err(KeyringError::Ambiguous), Some(Kind::MultiChain)) => Ok(Some(PhraseKind::Bip39)),
            (Err(KeyringError::Ambiguous), Some(Kind::Ton)) => Ok(Some(PhraseKind::Ton)),
            (Err(KeyringError::Ambiguous), None) => Err(text::CHOOSE_KIND.to_owned()),
            (Err(err), _) => Err(text::sentence(&err.to_string())),
        }
    }
}

impl Screen for Import {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Phrase, value) => {
                self.phrase = Secret::new(value);
                self.kind = None;
                self.recheck();
            }
            Action::Type(Field::Passphrase, value) => {
                self.passphrase = Secret::new(value);
                self.recheck();
            }
            Action::Pick(Pick::Kind(kind)) => self.kind = Some(*kind),
            Action::Submit => match self.chosen() {
                Err(problem) => self.error = Some(problem),
                Ok(kind) if self.adding => {
                    return Reply::call(Call::Import(
                        super::secret(&self.phrase),
                        super::secret(&self.passphrase),
                        kind,
                    ));
                }
                Ok(kind) => {
                    let next = Next::Import(self.phrase.clone(), self.passphrase.clone(), kind);
                    return Reply::nav(Nav::Push(Box::new(NewPassword::new(next))));
                }
            },
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Imported(Ok(_)) => {
                Reply::nav(Nav::Finish).with_note(Level::Success, text::IMPORTED)
            }
            Outcome::Imported(Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        let words = self.phrase.words().count();
        let status = match &self.check {
            _ if words == 0 => None,
            Ok(PhraseKind::Bip39) => Some(note(Level::Success, text::looks_multi_chain(words))),
            Ok(PhraseKind::Ton) => Some(note(Level::Success, text::LOOKS_TON)),
            Err(KeyringError::Ambiguous) => Some(note(Level::Info, text::AMBIGUOUS)),
            Err(err) => Some(note(Level::Warning, text::sentence(&err.to_string()))),
        };
        let mut form = vec![
            hidden(Field::Phrase, text::PHRASE, &self.phrase).into(),
            hidden(Field::Passphrase, text::PASSPHRASE, &self.passphrase).into(),
        ];
        if let Err(KeyringError::Ambiguous) = self.check {
            form.push(
                group(
                    text::WHICH_APP,
                    vec![
                        choice(
                            text::KIND_MULTI_CHAIN,
                            Action::Pick(Pick::Kind(Kind::MultiChain)),
                            self.kind == Some(Kind::MultiChain),
                        ),
                        choice(
                            text::KIND_TON,
                            Action::Pick(Pick::Kind(Kind::Ton)),
                            self.kind == Some(Kind::Ton),
                        ),
                    ],
                )
                .into(),
            );
        }
        Page {
            id: PageId::Import,
            title: text::IMPORT_TITLE.to_owned(),
            text: Some(text::IMPORT_TEXT.to_owned()),
            form,
            notes: status.into_iter().collect(),
            error: self.error.clone(),
            buttons: vec![button(text::CONTINUE, Action::Submit)],
            ..Page::default()
        }
    }
}

struct ImportKey {
    adding: bool,
    key: Secret,
    family: Family,
    error: Option<String>,
}

impl ImportKey {
    fn new(adding: bool) -> Self {
        Self {
            adding,
            key: Secret::default(),
            family: Family::default(),
            error: None,
        }
    }

    fn check(&self) -> Option<Note> {
        if self.key.expose().trim().is_empty() {
            return None;
        }
        Some(match bahaa_engine::check_key(self.key.expose()) {
            Ok(()) => note(Level::Success, text::KEY_OK),
            Err(err) => note(Level::Error, text::sentence(&err.to_string())),
        })
    }
}

impl Screen for ImportKey {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Key, value) => {
                self.key = Secret::new(value);
                self.error = None;
            }
            Action::Pick(Pick::Family(family)) => self.family = *family,
            Action::Submit => match bahaa_engine::check_key(self.key.expose()) {
                Err(err) => self.error = Some(text::sentence(&err.to_string())),
                Ok(()) if self.adding => {
                    return Reply::call(Call::ImportKey(
                        super::secret(&self.key),
                        chain(self.family),
                    ));
                }
                Ok(()) => {
                    let next = Next::ImportKey(self.key.clone(), self.family);
                    return Reply::nav(Nav::Push(Box::new(NewPassword::new(next))));
                }
            },
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Imported(Ok(_)) => {
                Reply::nav(Nav::Finish).with_note(Level::Success, text::IMPORTED)
            }
            Outcome::Imported(Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        let families = [
            (Family::Evm, text::EVM_NETWORKS),
            (Family::Tron, text::TRON),
        ]
        .into_iter()
        .map(|(family, label)| {
            choice(
                label,
                Action::Pick(Pick::Family(family)),
                self.family == family,
            )
        })
        .collect();
        Page {
            id: PageId::ImportKey,
            title: text::IMPORT_KEY.to_owned(),
            text: Some(text::IMPORT_KEY_TEXT.to_owned()),
            form: vec![
                Input {
                    hint: self.check(),
                    ..hidden(Field::Key, text::PRIVATE_KEY, &self.key)
                }
                .into(),
                group(text::USE_FOR, families).into(),
            ],
            error: self.error.clone(),
            buttons: vec![button(text::CONTINUE, Action::Submit)],
            ..Page::default()
        }
    }
}

fn chain(family: Family) -> ChainFamily {
    match family {
        Family::Evm => ChainFamily::Evm,
        Family::Tron => ChainFamily::Tron,
    }
}

fn new_phrase(words: Words) -> NewPhrase {
    match words {
        Words::Twelve => NewPhrase::Twelve,
        Words::TwentyFour => NewPhrase::TwentyFour,
    }
}

fn pick_positions(count: usize) -> [usize; CHECKED_WORDS] {
    let mut positions = [0; CHECKED_WORDS];
    let mut taken = 0;
    while taken < CHECKED_WORDS.min(count) {
        let mut random = [0u8; 8];
        if getrandom::fill(&mut random).is_err() {
            positions[taken] = taken;
            taken += 1;
            continue;
        }
        let span = u64::try_from(count).unwrap_or(u64::MAX);
        let position = usize::try_from(u64::from_le_bytes(random) % span).unwrap_or(0);
        if !positions[..taken].contains(&position) {
            positions[taken] = position;
            taken += 1;
        }
    }
    positions.sort_unstable();
    positions
}

pub fn completions(prefix: &str) -> Vec<&'static str> {
    const SHOWN: usize = 6;
    let prefix = prefix.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        return Vec::new();
    }
    bahaa_engine::english_words()
        .iter()
        .copied()
        .filter(|word| word.starts_with(&prefix))
        .take(SHOWN)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_positions_are_distinct_and_in_range() {
        for _ in 0..200 {
            let positions = pick_positions(12);
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(positions.iter().all(|position| *position < 12));
        }
    }

    #[test]
    fn completions_follow_the_word_list() {
        assert_eq!(completions("aba"), ["abandon"]);
        assert_eq!(completions("zo"), ["zone", "zoo"]);
        assert_eq!(completions(" "), Vec::<&str>::new());
        assert_eq!(completions("ab").len(), 6);
    }
}
