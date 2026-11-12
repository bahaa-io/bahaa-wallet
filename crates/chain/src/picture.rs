use std::io::Cursor;

use bahaa_types::{MAX_PICTURE, Picture};
use image::{ImageFormat, ImageReader, Limits};

pub const MAX_PICTURE_FILE: usize = 8 * 1024 * 1024;
const MAX_SIDE: u32 = 8_192;
const MAX_ALLOC: u64 = 128 * 1024 * 1024;
const FORMATS: [ImageFormat; 4] = [
    ImageFormat::Png,
    ImageFormat::Jpeg,
    ImageFormat::Gif,
    ImageFormat::WebP,
];

pub fn read_picture(bytes: &[u8]) -> Option<Picture> {
    let format = image::guess_format(bytes)
        .ok()
        .filter(|format| FORMATS.contains(format))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let side = u32::from(MAX_PICTURE);
    let decoded = reader.decode().ok()?;
    let small = if decoded.width() > side || decoded.height() > side {
        decoded.thumbnail(side, side)
    } else {
        decoded
    }
    .into_rgba8();
    let (width, height) = small.dimensions();
    Picture::new(
        u16::try_from(width).ok()?,
        u16::try_from(height).ok()?,
        small.into_raw(),
    )
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgba};

    use super::*;

    fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = ImageBuffer::from_fn(width, height, |x, y| {
            Rgba([
                u8::try_from(x % 256).unwrap(),
                u8::try_from(y % 256).unwrap(),
                200,
                255,
            ])
        });
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn pictures_shrink_to_fit_and_keep_their_shape() {
        let wide = read_picture(&encoded(400, 200, ImageFormat::Png)).unwrap();
        assert_eq!((wide.width(), wide.height()), (64, 32));
        let small = read_picture(&encoded(24, 20, ImageFormat::Png)).unwrap();
        assert_eq!((small.width(), small.height()), (24, 20));
        assert_eq!(small.pixel(5, 7), [5, 7, 200, 255]);
        let photo = read_picture(&encoded(90, 300, ImageFormat::Jpeg)).unwrap();
        assert_eq!(photo.height(), MAX_PICTURE);
        assert!(photo.width() < 20);
        let gif = read_picture(&encoded(64, 64, ImageFormat::Gif)).unwrap();
        assert_eq!(gif.width(), 64);
    }

    #[test]
    fn only_known_formats_within_limits_are_read() {
        assert!(read_picture(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_none());
        assert!(read_picture(&[]).is_none());
        let mut broken = encoded(40, 40, ImageFormat::Png);
        broken.truncate(broken.len() / 2);
        assert!(read_picture(&broken).is_none());
        let mut huge = encoded(2, 2, ImageFormat::Png);
        huge[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(read_picture(&huge).is_none());
    }
}
