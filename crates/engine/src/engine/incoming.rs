use std::cmp::Reverse;

use bahaa_store::{Received, Scan};
use bahaa_types::Address;

use super::{EngineError, Event, Worker};
use crate::KeyringError;
use crate::caution::{self, Arrival};
use crate::portfolio;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IncomingHistory {
    pub transfers: Vec<Arrival>,
    pub coverage: Vec<Scan>,
}

impl Worker {
    pub(super) fn record_scans(&mut self, scans: Vec<(Scan, Vec<Received>)>) {
        let mut fresh = Vec::new();
        for (scan, found) in scans {
            if self.store.save_scan(&scan, &found).is_err() {
                continue;
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
    }

    pub(super) fn incoming(&self) -> Result<IncomingHistory, EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let ours = |address| {
            self.accounts
                .iter()
                .any(|account| account.address == address)
        };
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

    pub(super) fn arrivals(&self, entries: &[Received]) -> Vec<Arrival> {
        let known = self.known();
        let snapshot = self.prices.borrow().clone();
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
                caution::arrival(entry.clone(), &known, value)
            })
            .collect()
    }
}

pub(super) fn sort(entries: &mut [Received]) {
    entries.sort_by_key(|entry| Reverse((entry.time, entry.block, entry.index)));
}

fn same(a: &Received, b: &Received) -> bool {
    a.network == b.network && a.id == b.id && a.index == b.index
}
