use qrcode::{EcLevel, QrCode};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme;
use crate::wrap;

const LOWER_HALF: &str = "▄";
const BLANK: &str = " ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scale {
    Half,
    Full,
}

impl Scale {
    fn quiet(self) -> usize {
        match self {
            Self::Half => 4,
            Self::Full => 2,
        }
    }
}

pub(crate) struct Code {
    size: usize,
    dark: Vec<bool>,
}

impl Code {
    pub(crate) fn new(text: &str) -> Option<Self> {
        let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::L).ok()?;
        Some(Self {
            size: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect(),
        })
    }

    fn modules(&self, scale: Scale) -> usize {
        self.size + 2 * scale.quiet()
    }

    pub(crate) fn width(&self, scale: Scale) -> u16 {
        wrap::cells(match scale {
            Scale::Half => self.modules(scale),
            Scale::Full => 2 * self.modules(scale),
        })
    }

    pub(crate) fn height(&self, scale: Scale) -> u16 {
        wrap::cells(match scale {
            Scale::Half => self.modules(scale).div_ceil(2),
            Scale::Full => self.modules(scale),
        })
    }

    pub(crate) fn fits(&self, scale: Scale, width: u16, height: u16) -> bool {
        self.width(scale) <= width && self.height(scale) <= height
    }

    pub(crate) fn scale(&self, width: u16, height: u16) -> Option<Scale> {
        [Scale::Full, Scale::Half]
            .into_iter()
            .find(|scale| self.fits(*scale, width, height))
    }

    pub(crate) fn draw(&self, buffer: &mut Buffer, area: Rect, scale: Scale) {
        for row in 0..area.height {
            for column in 0..area.width {
                let (x, y) = (usize::from(column), usize::from(row));
                let (top, bottom) = match scale {
                    Scale::Half => (self.color(x, 2 * y, scale), self.color(x, 2 * y + 1, scale)),
                    Scale::Full => {
                        let color = self.color(x / 2, y, scale);
                        (color, color)
                    }
                };
                let symbol = if top == bottom { BLANK } else { LOWER_HALF };
                buffer[(area.x + column, area.y + row)]
                    .set_symbol(symbol)
                    .set_style(Style::new().fg(bottom).bg(top));
            }
        }
    }

    fn color(&self, x: usize, y: usize, scale: Scale) -> Color {
        if self.module(x, y, scale.quiet()) {
            theme::QR_DARK
        } else {
            theme::QR_LIGHT
        }
    }

    fn module(&self, x: usize, y: usize, quiet: usize) -> bool {
        let inside = |at: usize| at.checked_sub(quiet).filter(|at| *at < self.size);
        match (inside(x), inside(y)) {
            (Some(x), Some(y)) => self.dark[y * self.size + x],
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use super::{BLANK, Code, LOWER_HALF, Scale};
    use crate::theme;

    const ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    #[test]
    fn codes_have_a_light_border_and_two_sizes() {
        let code = Code::new(ADDRESS).unwrap();
        assert_eq!(
            (code.width(Scale::Half), code.height(Scale::Half)),
            (37, 19)
        );
        assert_eq!(
            (code.width(Scale::Full), code.height(Scale::Full)),
            (66, 33)
        );
        assert!(!code.module(3, 3, 4) && !code.module(0, 36, 4));
        assert!(code.module(4, 4, 4) && code.module(2, 2, 2));
        assert_eq!(code.scale(80, 40), Some(Scale::Full));
        assert_eq!(code.scale(80, 30), Some(Scale::Half));
        assert_eq!(code.scale(30, 30), None);
        assert!(code.fits(Scale::Half, 37, 19) && !code.fits(Scale::Half, 36, 19));
    }

    #[test]
    fn half_blocks_are_used_only_where_a_cell_holds_two_colors() {
        let code = Code::new(ADDRESS).unwrap();
        let area = Rect::new(0, 0, code.width(Scale::Half), code.height(Scale::Half));
        let mut buffer = Buffer::empty(area);
        code.draw(&mut buffer, area, Scale::Half);
        let light = &buffer[(0, 0)];
        assert_eq!((light.symbol(), light.bg), (BLANK, theme::QR_LIGHT));
        let dark = &buffer[(4, 2)];
        assert_eq!((dark.symbol(), dark.bg), (BLANK, theme::QR_DARK));
        let edge = &buffer[(4, 5)];
        assert_eq!(
            (edge.symbol(), edge.fg, edge.bg),
            (LOWER_HALF, theme::QR_LIGHT, theme::QR_DARK)
        );
    }
}
