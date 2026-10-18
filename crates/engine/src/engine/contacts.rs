use bahaa_chain::ChainError;
use bahaa_store::{Contact, Received};
use bahaa_types::{Address, NetworkId};

use super::{EngineError, Event, Worker};
use crate::KeyringError;
use crate::caution::{self, Caution};

impl Worker {
    pub(super) fn contacts(&self) -> Result<Vec<Contact>, EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let mut contacts = self.store.contacts()?;
        contacts.sort_by(|a, b| {
            (a.name.to_lowercase(), a.network.as_str())
                .cmp(&(b.name.to_lowercase(), b.network.as_str()))
        });
        Ok(contacts)
    }

    pub(super) fn save_contact(&mut self, contact: &Contact) -> Result<(), EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let network = self
            .registry
            .get(&contact.network)
            .ok_or(EngineError::UnknownNetwork)?;
        if network.family() != contact.address.family() {
            return Err(ChainError::Invalid("this address is for another kind of network").into());
        }
        self.store.save_contact(contact)?;
        self.contacts_changed()
    }

    pub(super) fn remove_contact(
        &mut self,
        network: &NetworkId,
        address: &Address,
    ) -> Result<bool, EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let removed = self.store.remove_contact(network, address)?;
        if removed {
            self.contacts_changed()?;
        }
        Ok(removed)
    }

    pub(super) fn cautions(&self, address: &Address) -> Result<Vec<Caution>, EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let senders: Vec<Received> = self
            .received
            .iter()
            .filter(|entry| entry.from == *address)
            .cloned()
            .collect();
        Ok(caution::recipient(
            address,
            &self.known(),
            &self.arrivals(&senders),
        ))
    }

    fn contacts_changed(&self) -> Result<(), EngineError> {
        let _ = self.events.send(Event::ContactsChanged(self.contacts()?));
        Ok(())
    }
}
