use bahaa_config::Registry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fake {
    Copies(String),
    WebAddress,
    Bait,
}

const MIN_COPIED: usize = 6;
const BAIT: [&str; 11] = [
    "airdrop",
    "airdrops",
    "claim",
    "claimable",
    "eligible",
    "giveaway",
    "redeem",
    "reward",
    "rewards",
    "voucher",
    "vouchers",
];
const WEB: [&str; 36] = [
    "ai", "app", "bet", "cash", "cc", "click", "club", "co", "com", "fi", "finance", "fun", "gg",
    "gift", "gifts", "info", "io", "link", "live", "me", "money", "net", "network", "online",
    "org", "pro", "promo", "shop", "site", "store", "tech", "to", "top", "vip", "win", "xyz",
];

pub(super) struct Known<'a> {
    names: Vec<(String, &'a str)>,
}

impl<'a> Known<'a> {
    pub(super) fn new(registry: &'a Registry) -> Self {
        let mut names: Vec<(String, &'a str)> = registry
            .networks()
            .flat_map(|network| &network.collections)
            .map(|known| (squeezed(&known.name), known.name.as_str()))
            .filter(|(squeezed, _)| squeezed.chars().count() >= MIN_COPIED)
            .collect();
        names.sort_unstable();
        names.dedup_by(|a, b| a.0 == b.0);
        Self { names }
    }

    pub(super) fn fake<'t>(
        &self,
        texts: impl IntoIterator<Item = &'t str> + Clone,
    ) -> Option<Fake> {
        let copied = texts.clone().into_iter().find_map(|text| {
            let text = squeezed(text);
            self.names
                .iter()
                .find(|(name, _)| text.contains(name.as_str()))
                .map(|(_, name)| Fake::Copies((*name).to_owned()))
        });
        copied
            .or_else(|| {
                texts
                    .clone()
                    .into_iter()
                    .any(web_address)
                    .then_some(Fake::WebAddress)
            })
            .or_else(|| texts.into_iter().any(bait).then_some(Fake::Bait))
    }
}

fn squeezed(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn web_address(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("://") || lower.contains("www.") {
        return true;
    }
    lower
        .split(|c: char| c.is_whitespace() || "()[]{}<>\"'!?|,;*".contains(c))
        .any(|word| {
            let host = word.split('/').next().unwrap_or_default();
            let host = host.trim_end_matches(['.', ':']);
            host.rsplit_once('.').is_some_and(|(name, ending)| {
                name.chars().next_back().is_some_and(char::is_alphanumeric) && WEB.contains(&ending)
            })
        })
}

fn bait(text: &str) -> bool {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| BAIT.contains(&word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judge(texts: &[&str]) -> Option<Fake> {
        let registry = Registry::builtin().unwrap();
        Known::new(&registry).fake(texts.iter().copied())
    }

    #[test]
    fn names_of_known_collections_are_not_borrowed() {
        for copy in [
            "Bored Ape Yacht Club",
            "BoredApeYachtClub V2",
            "bored-ape-yacht-club airdrop",
            "Pudgy Penguins Rewards",
            "MAD LADS #1",
        ] {
            assert!(matches!(judge(&[copy]), Some(Fake::Copies(_))), "{copy}");
        }
        assert_eq!(
            judge(&["Mutant Ape Yacht Club"]),
            Some(Fake::Copies("Mutant Ape Yacht Club".to_owned()))
        );
        assert_eq!(judge(&["Azuki Garden"]), None);
    }

    #[test]
    fn web_addresses_and_bait_words_are_spam() {
        for spam in [
            "2400JUP Lucky Case JUPBOX.io",
            "Visit www.example.org",
            "https://scam.example",
            "Join t.me/scam",
            "Get yours at claim-eth.xyz.",
        ] {
            assert_eq!(judge(&[spam]), Some(Fake::WebAddress), "{spam}");
        }
        for spam in ["$5000 USDC Reward", "Airdrop Pass", "CLAIM your ETH"] {
            assert_eq!(judge(&[spam]), Some(Fake::Bait), "{spam}");
        }
        for fine in [
            "vitalik.eth",
            "durov.ton",
            "@durov",
            "Plush Pepe #123",
            "Noun 1.5",
            "Chromie Squiggle #1",
            "Reclaimed Land",
        ] {
            assert_eq!(judge(&[fine]), None, "{fine}");
        }
        assert_eq!(
            judge(&["Plain Cats", "Win at cats.win"]),
            Some(Fake::WebAddress)
        );
    }
}
