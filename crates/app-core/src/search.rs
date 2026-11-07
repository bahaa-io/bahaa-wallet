use bahaa_engine::{Address, Amount, AssetId, ChainFamily, Movement, NetworkId, TonForm, WalletId};

use crate::core::Ctx;
use crate::parse;

const FRAGMENT: usize = 3;

enum Term {
    Address(Address),
    Word(String),
}

pub(crate) struct Query {
    terms: Vec<Term>,
}

#[derive(Default)]
pub(crate) struct Facts {
    words: Vec<String>,
    forms: Vec<String>,
    addresses: Vec<Address>,
    amounts: Vec<String>,
}

impl Query {
    pub(crate) fn new(ctx: &Ctx<'_>, text: &str) -> Self {
        Self::parse(&ctx.families(), text)
    }

    fn parse(families: &[ChainFamily], text: &str) -> Self {
        let terms = text
            .split_whitespace()
            .map(|term| match parse::any_address(term, families) {
                Ok(address) => Term::Address(address),
                Err(_) => Term::Word(term.to_lowercase()),
            })
            .collect();
        Self { terms }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub(crate) fn matches(&self, facts: &Facts) -> bool {
        self.terms.iter().all(|term| match term {
            Term::Address(address) => facts.addresses.contains(address),
            Term::Word(word) => {
                facts
                    .words
                    .iter()
                    .any(|known| known.contains(word.as_str()))
                    || facts
                        .amounts
                        .iter()
                        .any(|amount| amount.starts_with(word.as_str()))
                    || (word.chars().count() >= FRAGMENT
                        && facts.forms.iter().any(|form| form.contains(word.as_str())))
            }
        })
    }
}

impl Facts {
    pub(crate) fn word(&mut self, text: &str) -> &mut Self {
        self.words.push(text.to_lowercase());
        self
    }

    pub(crate) fn id(&mut self, text: &str) -> &mut Self {
        self.forms.push(text.to_lowercase());
        self
    }

    pub(crate) fn amount(&mut self, amount: Amount) -> &mut Self {
        self.amounts.push(amount.to_string());
        self
    }

    pub(crate) fn network(&mut self, ctx: &Ctx<'_>, network: &NetworkId) -> &mut Self {
        self.word(network.as_str()).word(&ctx.network_name(network))
    }

    pub(crate) fn asset(&mut self, ctx: &Ctx<'_>, asset: &AssetId) -> &mut Self {
        self.word(&ctx.symbol(asset));
        if let Some(coin) = ctx.coin(asset) {
            self.word(&coin.name);
        }
        self.network(ctx, asset.network())
    }

    pub(crate) fn wallet(
        &mut self,
        ctx: &Ctx<'_>,
        wallet: WalletId,
        address: &Address,
    ) -> &mut Self {
        self.word(&ctx.owner_label(wallet, address))
    }

    pub(crate) fn moved(&mut self, ctx: &Ctx<'_>, moved: &Movement) -> &mut Self {
        self.amount(moved.amount)
            .asset(ctx, &moved.asset)
            .address(ctx, &moved.from, &moved.network)
            .address(ctx, &moved.to, &moved.network)
            .id(&moved.id)
    }

    pub(crate) fn address(
        &mut self,
        ctx: &Ctx<'_>,
        address: &Address,
        network: &NetworkId,
    ) -> &mut Self {
        self.addresses.push(*address);
        if let Some(contact) = ctx.contact(network, address) {
            self.words.push(contact.name.to_lowercase());
        }
        self.forms.push(address.to_string().to_lowercase());
        self.forms.push(ctx.shown(address, network).to_lowercase());
        for bounceable in [true, false] {
            for testnet in [false, true] {
                if let Some(form) = address.ton_friendly(TonForm {
                    bounceable,
                    testnet,
                }) {
                    self.forms.push(form.to_lowercase());
                }
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TON: &str = "UQBEgrmXc1WWtt4qIAR-hvQHBlsYAULFCqunQ3Ollcsrb-T_";
    const EVM: &str = "0xa2b53fB8d860BF379DC72d9c2a5C533CED61F341";

    fn query(text: &str) -> Query {
        Query::parse(&[ChainFamily::Evm, ChainFamily::Ton], text)
    }

    fn facts() -> Facts {
        let ton = Address::parse(ChainFamily::Ton, TON).unwrap();
        let mut facts = Facts::default();
        facts
            .word("USDT")
            .word("Tether USD")
            .word("ton")
            .word("TON");
        facts.amount(Amount::parse("100.25", 6).unwrap());
        facts.addresses.push(ton);
        facts.forms.push(TON.to_lowercase());
        facts.id("9064ca50b1f3");
        facts
    }

    #[test]
    fn every_word_must_match_and_case_does_not_matter() {
        assert!(query("usdt ton").matches(&facts()));
        assert!(query("Tether").matches(&facts()));
        assert!(!query("usdt bsc").matches(&facts()));
        assert!(query("").matches(&facts()));
        assert!(query("   ").is_empty());
    }

    #[test]
    fn amounts_match_from_their_start() {
        assert!(query("100").matches(&facts()));
        assert!(query("100.2").matches(&facts()));
        assert!(!query("0.25").matches(&facts()));
        assert!(!query("25").matches(&facts()));
    }

    #[test]
    fn short_words_do_not_match_inside_addresses_and_ids() {
        assert!(query("9064").matches(&facts()));
        assert!(query("uqbeg").matches(&facts()));
        assert!(!query("uq").matches(&facts()));
        assert!(!query("90").matches(&facts()));
    }

    #[test]
    fn a_full_address_matches_in_every_form() {
        let ton = Address::parse(ChainFamily::Ton, TON).unwrap();
        let bounceable = ton
            .ton_friendly(TonForm {
                bounceable: true,
                testnet: false,
            })
            .unwrap();
        for form in [TON.to_owned(), bounceable, ton.to_string()] {
            assert!(query(&form).matches(&facts()), "{form}");
        }
        let mut evm = Facts::default();
        evm.addresses
            .push(Address::parse(ChainFamily::Evm, EVM).unwrap());
        assert!(query(&EVM.to_lowercase()).matches(&evm));
        assert!(!query(&EVM.to_lowercase()).matches(&facts()));
    }
}
