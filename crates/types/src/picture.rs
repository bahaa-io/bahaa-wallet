use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::hex;

pub const MAX_PICTURE: u16 = 64;
const CHANNELS: usize = 4;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Stored", into = "Stored")]
pub struct Picture {
    width: u16,
    height: u16,
    pixels: Arc<[u8]>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    width: u16,
    height: u16,
    pixels: String,
}

impl Picture {
    pub fn new(width: u16, height: u16, pixels: Vec<u8>) -> Option<Self> {
        let sized = |side: u16| (1..=MAX_PICTURE).contains(&side);
        let expected = usize::from(width) * usize::from(height) * CHANNELS;
        (sized(width) && sized(height) && pixels.len() == expected).then(|| Self {
            width,
            height,
            pixels: pixels.into(),
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn pixel(&self, x: u16, y: u16) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let at = (usize::from(y) * usize::from(self.width) + usize::from(x)) * CHANNELS;
        let mut pixel = [0; 4];
        pixel.copy_from_slice(&self.pixels[at..at + CHANNELS]);
        pixel
    }
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Picture({}x{})", self.width, self.height)
    }
}

impl TryFrom<Stored> for Picture {
    type Error = &'static str;

    fn try_from(stored: Stored) -> Result<Self, Self::Error> {
        let pixels = hex::decode(&stored.pixels).ok_or("picture pixels are not hex")?;
        Self::new(stored.width, stored.height, pixels).ok_or("not a picture of the allowed size")
    }
}

impl From<Picture> for Stored {
    fn from(picture: Picture) -> Self {
        Self {
            width: picture.width,
            height: picture.height,
            pixels: hex::encode(&picture.pixels),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_keep_their_size_and_round_trip_as_hex() {
        let pixels: Vec<u8> = (0..2 * 3 * 4)
            .map(|byte| u8::try_from(byte).unwrap())
            .collect();
        let picture = Picture::new(2, 3, pixels).unwrap();
        assert_eq!(picture.pixel(1, 2), [20, 21, 22, 23]);
        assert_eq!(picture.pixel(2, 0), [0; 4]);
        let text = serde_json::to_string(&picture).unwrap();
        assert_eq!(serde_json::from_str::<Picture>(&text).unwrap(), picture);
        assert_eq!(format!("{picture:?}"), "Picture(2x3)");
        for (width, height, len) in [(0, 1, 0), (65, 1, 260), (2, 2, 15), (2, 2, 17)] {
            assert!(Picture::new(width, height, vec![0; len]).is_none());
        }
        let edited = text.replace("\"width\":2", "\"width\":3");
        assert!(serde_json::from_str::<Picture>(&edited).is_err());
    }
}
