use arboard::Clipboard;
use zeroize::Zeroizing;

use crate::hints;

pub(crate) struct Platform {
    clipboard: Option<Clipboard>,
    copied: Option<Zeroizing<String>>,
}

impl Platform {
    pub(crate) fn new() -> Self {
        Self {
            clipboard: Clipboard::new().ok(),
            copied: None,
        }
    }

    pub(crate) fn copy(&mut self, text: String) -> Result<(), &'static str> {
        let text = Zeroizing::new(text);
        let clipboard = self.clipboard.as_mut().ok_or(hints::NO_CLIPBOARD)?;
        clipboard
            .set_text(text.as_str())
            .map_err(|_| hints::NO_CLIPBOARD)?;
        self.copied = Some(text);
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        if let Some(clipboard) = self.clipboard.as_mut()
            && let Some(copied) = self.copied.take()
            && clipboard
                .get_text()
                .is_ok_and(|current| *Zeroizing::new(current) == *copied)
        {
            let _ = clipboard.clear();
        }
    }

    pub(crate) fn open(url: &str) -> Result<(), &'static str> {
        open::that_detached(url).map_err(|_| hints::NO_BROWSER)
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.clear();
    }
}
