use bahaa_chain::ChainError;
use bahaa_store::Contact;
use bahaa_types::{Address, NetworkId};

use super::{EngineError, Event, Worker};
use crate::caution::Caution;

impl Worker {
    pub(super) fn contacts(&self) -> Result<Vec<Contact>, EngineError> {
        self.ensure_unlocked()?;
        let mut contacts = self.store.contacts()?;
        contacts
            .sort_by_cached_key(|contact| (contact.name.to_lowercase(), contact.network.clone()));
        Ok(contacts)
    }

    pub(super) fn save_contact(&mut self, contact: &Contact) -> Result<(), EngineError> {
        self.ensure_unlocked()?;
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
        self.ensure_unlocked()?;
        let removed = self.store.remove_contact(network, address)?;
        if removed {
            self.contacts_changed()?;
        }
        Ok(removed)
    }

    pub(super) fn cautions(&self, address: &Address) -> Result<Vec<Caution>, EngineError> {
        self.ensure_unlocked()?;
        Ok(self.recipient_cautions(address))
    }

    fn contacts_changed(&self) -> Result<(), EngineError> {
        let _ = self.events.send(Event::ContactsChanged(self.contacts()?));
        Ok(())
    }
}
