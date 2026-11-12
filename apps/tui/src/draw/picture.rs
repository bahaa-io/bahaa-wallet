use bahaa_app_core::Picture;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme;

const LOWER_HALF: &str = "▄";
const BLANK: &str = " ";
const OPAQUE: u32 = 128;

pub(crate) fn fit(picture: &Picture, columns: u16, rows: u16) -> Option<(u16, u16)> {
    let (width, height) = (u32::from(picture.width()), u32::from(picture.height()));
    let (room_wide, room_high) = (u32::from(columns), 2 * u32::from(rows));
    if room_wide == 0 || room_high == 0 {
        return None;
    }
    let (wide, high) = if width * room_high <= height * room_wide {
        ((width * room_high / height).max(1), room_high)
    } else {
        (room_wide, (height * room_wide / width).max(1))
    };
    Some((
        u16::try_from(wide).ok()?,
        u16::try_from(high.div_ceil(2)).ok()?,
    ))
}

pub(crate) fn draw(picture: &Picture, buffer: &mut Buffer, area: Rect, back: Color) {
    let high = 2 * area.height;
    for row in 0..area.height {
        for column in 0..area.width {
            let top = sample(picture, column, 2 * row, area.width, high).unwrap_or(back);
            let bottom = sample(picture, column, 2 * row + 1, area.width, high).unwrap_or(back);
            let symbol = if top == bottom { BLANK } else { LOWER_HALF };
            buffer[(area.x + column, area.y + row)]
                .set_symbol(symbol)
                .set_style(Style::new().fg(bottom).bg(top));
        }
    }
}

fn sample(picture: &Picture, x: u16, y: u16, wide: u16, high: u16) -> Option<Color> {
    let span = |at: u16, out: u16, source: u16| {
        let (at, out, source) = (u32::from(at), u32::from(out), u32::from(source));
        let start = at * source / out;
        let end = ((at + 1) * source / out).max(start + 1).min(source);
        (start, end)
    };
    let (left, right) = span(x, wide, picture.width());
    let (upper, lower) = span(y, high, picture.height());
    let mut sums = [0u32; 3];
    let (mut alpha, mut count) = (0u32, 0u32);
    for row in upper..lower {
        for column in left..right {
            let pixel = picture.pixel(u16::try_from(column).ok()?, u16::try_from(row).ok()?);
            let weight = u32::from(pixel[3]);
            for (sum, channel) in sums.iter_mut().zip(pixel) {
                *sum += u32::from(channel) * weight;
            }
            alpha += weight;
            count += 1;
        }
    }
    if count == 0 || alpha < OPAQUE * count {
        return None;
    }
    let [red, green, blue] = sums.map(|sum| u8::try_from(sum / alpha).unwrap_or(u8::MAX));
    Some(theme::rgb([red, green, blue]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u16, height: u16, pixel: impl Fn(u16, u16) -> [u8; 4]) -> Picture {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend(pixel(x, y));
            }
        }
        Picture::new(width, height, pixels).unwrap()
    }

    #[test]
    fn pictures_fit_the_room_and_keep_their_shape() {
        let square = picture(64, 64, |_, _| [0, 0, 0, 255]);
        assert_eq!(fit(&square, 32, 30), Some((32, 16)));
        assert_eq!(fit(&square, 80, 10), Some((20, 10)));
        let tall = picture(20, 60, |_, _| [0, 0, 0, 255]);
        assert_eq!(fit(&tall, 32, 15), Some((10, 15)));
        assert_eq!(fit(&tall, 0, 15), None);
        let punk = picture(24, 24, |_, _| [0, 0, 0, 255]);
        assert_eq!(fit(&punk, 48, 24), Some((48, 24)));
    }

    #[test]
    fn two_pixels_share_a_cell_and_clear_ones_show_the_background() {
        let striped = picture(2, 2, |_, y| {
            if y == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 0]
            }
        });
        let area = Rect::new(0, 0, 2, 1);
        let mut buffer = Buffer::empty(area);
        let back = theme::rgb([1, 2, 3]);
        draw(&striped, &mut buffer, area, back);
        let cell = &buffer[(0, 0)];
        assert_eq!(
            (cell.symbol(), cell.bg, cell.fg),
            (LOWER_HALF, theme::rgb([255, 0, 0]), back)
        );
        let solid = picture(1, 1, |_, _| [9, 9, 9, 255]);
        let mut buffer = Buffer::empty(area);
        draw(&solid, &mut buffer, area, back);
        assert_eq!(buffer[(1, 0)].symbol(), BLANK);
        assert_eq!(buffer[(1, 0)].bg, theme::rgb([9, 9, 9]));
    }

    #[test]
    fn shrinking_averages_and_growing_repeats() {
        let checker = picture(2, 2, |x, y| {
            if (x + y) % 2 == 0 {
                [200, 0, 0, 255]
            } else {
                [0, 0, 100, 255]
            }
        });
        assert_eq!(sample(&checker, 0, 0, 1, 1), Some(theme::rgb([100, 0, 50])));
        assert_eq!(sample(&checker, 3, 0, 4, 4), Some(theme::rgb([0, 0, 100])));
    }
}
