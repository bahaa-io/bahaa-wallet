use std::time::Duration;

use bahaa_market::{Quote, Snapshot};
use bahaa_store::{SavedPrices, SavedQuote, Store};
use tokio::time::Instant;

use super::Worker;

const SAVE_EVERY: Duration = Duration::from_mins(1);
const MAX_SAVED: usize = 512;

pub(super) fn saved(store: &Store, base: &str) -> Snapshot {
    let quotes = store
        .prices(base)
        .ok()
        .flatten()
        .map(|saved| {
            saved
                .quotes
                .into_iter()
                .map(|quote| {
                    let kept = Quote {
                        price: quote.price,
                        updated_at: quote.updated_at,
                    };
                    (quote.id, kept)
                })
                .collect()
        })
        .unwrap_or_default();
    Snapshot {
        base: base.to_owned(),
        quotes,
        ..Snapshot::default()
    }
}

impl Worker {
    pub(super) fn quotes(&self) -> Snapshot {
        let live = self.prices.borrow().clone();
        let base = self.settings.fiat.price_base();
        if self.saved.base != base {
            return live;
        }
        let mut merged = Snapshot {
            base: base.to_owned(),
            quotes: self.saved.quotes.clone(),
            status: live.status,
        };
        if live.base == base {
            merged.quotes.extend(live.quotes);
        }
        merged
    }

    pub(super) fn remember_prices(&mut self) {
        let live = self.prices.borrow().clone();
        let base = self.settings.fiat.price_base();
        if live.base != base || live.quotes.is_empty() {
            return;
        }
        if self.saved.base != base {
            self.saved = saved(&self.store, base);
        }
        let before = self.saved.quotes.clone();
        self.saved.quotes.extend(live.quotes);
        while self.saved.quotes.len() > MAX_SAVED {
            let oldest = self
                .saved
                .quotes
                .iter()
                .min_by_key(|(_, quote)| quote.updated_at)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.saved.quotes.remove(&oldest);
            }
        }
        self.unsaved |= self.saved.quotes != before;
        if self
            .saved_at
            .is_none_or(|saved_at| saved_at.elapsed() >= SAVE_EVERY)
        {
            self.keep_prices();
        }
    }

    pub(super) fn keep_prices(&mut self) {
        if !self.unsaved {
            return;
        }
        let prices = SavedPrices {
            base: self.saved.base.clone(),
            quotes: self
                .saved
                .quotes
                .iter()
                .map(|(id, quote)| SavedQuote {
                    id: id.clone(),
                    price: quote.price,
                    updated_at: quote.updated_at,
                })
                .collect(),
        };
        if self.store.save_prices(&prices).is_ok() {
            self.unsaved = false;
            self.saved_at = Some(Instant::now());
        }
    }

    pub(super) fn switch_prices(&mut self) {
        let base = self.settings.fiat.price_base();
        if self.saved.base != base {
            self.keep_prices();
            self.saved = saved(&self.store, base);
            self.saved_at = None;
        }
    }
}
