use std::cmp::Reverse;

use bahaa_store::{Movement, Outgoing, Received, Scan, SentTx, Store};
use bahaa_types::Address;

use super::{EngineError, Event, Worker};
use crate::caution::{self, Arrival, Caution};
use crate::portfolio;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IncomingHistory {
    pub transfers: Vec<Arrival>,
    pub coverage: Vec<Scan>,
}

impl Worker {
    pub(super) fn record_scans(&mut self, scans: Vec<(Scan, Vec<Received>, Vec<Outgoing>)>) {
        let mut fresh = Vec::new();
        let mut departed = Vec::new();
        let valid: Vec<_> = scans
            .into_iter()
            .filter(|(scan, found, sent)| Store::check_scan(scan, found, sent).is_ok())
            .collect();
        let batch: Vec<_> = valid
            .iter()
            .map(|(scan, found, sent)| (scan, found.as_slice(), sent.as_slice()))
            .collect();
        if self.store.save_scans(&batch).is_err() {
            return;
        }
        for (scan, found, sent) in valid {
            for entry in sent {
                if !self.outgoing.iter().any(|known| same(known, &entry)) {
                    self.outgoing.push(entry.clone());
                    departed.push(entry);
                }
            }
            match self
                .scans
                .iter_mut()
                .find(|known| known.network == scan.network && known.address == scan.address)
            {
                Some(known) => *known = scan,
                None => self.scans.push(scan),
            }
            for entry in found {
                if !self.received.iter().any(|known| same(known, &entry)) {
                    self.received.push(entry.clone());
                    fresh.push(entry);
                }
            }
        }
        if !fresh.is_empty() {
            sort(&mut self.received);
            sort(&mut fresh);
            let _ = self.events.send(Event::Received(self.arrivals(&fresh)));
        }
        let departed = elsewhere(&departed, &self.history);
        if !departed.is_empty() {
            sort(&mut self.outgoing);
            let _ = self.events.send(Event::SentElsewhere(departed));
        }
    }

    pub(super) fn outgoing(&self) -> Result<Vec<Outgoing>, EngineError> {
        self.ensure_unlocked()?;
        let mine: Vec<Outgoing> = self
            .outgoing
            .iter()
            .filter(|entry| self.owner(&entry.from).is_some())
            .cloned()
            .collect();
        Ok(elsewhere(&mine, &self.history))
    }

    pub(super) fn incoming(&self) -> Result<IncomingHistory, EngineError> {
        self.ensure_unlocked()?;
        let ours = |address: Address| self.owner(&address).is_some();
        let mine: Vec<Received> = self
            .received
            .iter()
            .filter(|entry| ours(entry.to))
            .cloned()
            .collect();
        Ok(IncomingHistory {
            transfers: self.arrivals(&mine),
            coverage: self
                .scans
                .iter()
                .filter(|scan| ours(scan.address))
                .cloned()
                .collect(),
        })
    }

    pub(super) fn known(&self) -> Vec<Address> {
        let mut known: Vec<Address> = self
            .accounts
            .iter()
            .map(|account| account.address)
            .chain(self.history.iter().map(|tx| tx.to))
            .chain(
                self.store
                    .contacts()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|contact| contact.address),
            )
            .collect();
        known.sort();
        known.dedup();
        known
    }

    pub(super) fn recipient_cautions(&self, to: &Address) -> Vec<Caution> {
        let known = self.known();
        let senders: Vec<Received> = self
            .received
            .iter()
            .filter(|entry| entry.from == *to)
            .cloned()
            .collect();
        caution::recipient(to, &known, &self.arrivals_among(&senders, &known))
    }

    pub(super) fn arrivals(&self, entries: &[Received]) -> Vec<Arrival> {
        self.arrivals_among(entries, &self.known())
    }

    fn arrivals_among(&self, entries: &[Received], known: &[Address]) -> Vec<Arrival> {
        let snapshot = self.quotes();
        entries
            .iter()
            .map(|entry| {
                let value = portfolio::value(
                    &self.registry,
                    &self.custom,
                    &snapshot,
                    self.settings.fiat,
                    &entry.asset,
                    entry.amount,
                );
                caution::arrival(entry.clone(), known, value)
            })
            .collect()
    }
}

pub(super) fn sort(entries: &mut [Movement]) {
    entries.sort_by_key(|entry| Reverse((entry.time, entry.block, entry.index)));
}

fn same(a: &Movement, b: &Movement) -> bool {
    a.network == b.network && a.id == b.id && a.index == b.index
}

fn elsewhere(entries: &[Outgoing], history: &[SentTx]) -> Vec<Outgoing> {
    entries
        .iter()
        .filter(|entry| {
            !history
                .iter()
                .any(|tx| tx.network == entry.network && tx.id.eq_ignore_ascii_case(&entry.id))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use bahaa_types::{Address, Amount};

    use super::*;

    fn moved(id: &str) -> Outgoing {
        Outgoing {
            id: id.to_owned(),
            index: 0,
            network: "bsc".parse().unwrap(),
            from: Address::Evm([1; 20]),
            to: Address::Evm([2; 20]),
            asset: "bsc".parse().unwrap(),
            amount: Amount::parse("1", 18).unwrap(),
            block: 5,
            time: 5,
            memo: None,
        }
    }

    fn ours(id: &str, network: &str) -> SentTx {
        SentTx {
            id: id.to_owned(),
            network: network.parse().unwrap(),
            from: Address::Evm([1; 20]),
            to: Address::Evm([2; 20]),
            asset: "bsc".parse().unwrap(),
            amount: Amount::parse("1", 18).unwrap(),
            sequence: None,
            fee_max: Amount::parse("0.01", 18).unwrap(),
            fee_paid: None,
            block: None,
            created_at: 1,
            expires_at: None,
            status: bahaa_store::SentStatus::Confirmed,
            raw: None,
            transaction: None,
            memo: None,
            replaces: None,
        }
    }

    #[test]
    fn sends_this_app_made_are_not_listed_again() {
        let seen = [moved("0xAB"), moved("0xcd")];
        let shown = elsewhere(&seen, &[ours("0xab", "bsc"), ours("0xcd", "ethereum")]);
        assert_eq!(shown, [moved("0xcd")]);
    }
}
