use qrcode::{EcLevel, QrCode};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme;
use crate::wrap;

const QUIET: usize = 4;
const HALF: &str = "▀";
const FULL: &str = " ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scale {
    Half,
    Full,
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

    fn modules(&self) -> usize {
        self.size + 2 * QUIET
    }

    pub(crate) fn width(&self, scale: Scale) -> u16 {
        wrap::cells(match scale {
            Scale::Half => self.modules(),
            Scale::Full => 2 * self.modules(),
        })
    }

    pub(crate) fn height(&self, scale: Scale) -> u16 {
        wrap::cells(match scale {
            Scale::Half => self.modules().div_ceil(2),
            Scale::Full => self.modules(),
        })
    }

    pub(crate) fn scale(&self, width: u16, height: u16) -> Option<Scale> {
        [Scale::Full, Scale::Half]
            .into_iter()
            .find(|scale| self.width(*scale) <= width && self.height(*scale) <= height)
    }

    pub(crate) fn draw(&self, buffer: &mut Buffer, area: Rect, scale: Scale) {
        for row in 0..area.height {
            for column in 0..area.width {
                let (x, y) = (usize::from(column), usize::from(row));
                let (symbol, style) = match scale {
                    Scale::Half => (
                        HALF,
                        Style::new()
                            .fg(self.color(x, 2 * y))
                            .bg(self.color(x, 2 * y + 1)),
                    ),
                    Scale::Full => {
                        let color = self.color(x / 2, y);
                        (FULL, Style::new().fg(color).bg(color))
                    }
                };
                buffer[(area.x + column, area.y + row)]
                    .set_symbol(symbol)
                    .set_style(style);
            }
        }
    }

    fn color(&self, x: usize, y: usize) -> Color {
        if self.module(x, y) {
            theme::QR_DARK
        } else {
            theme::QR_LIGHT
        }
    }

    fn module(&self, x: usize, y: usize) -> bool {
        let inside = |at: usize| at.checked_sub(QUIET).filter(|at| *at < self.size);
        match (inside(x), inside(y)) {
            (Some(x), Some(y)) => self.dark[y * self.size + x],
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Code, Scale};

    #[test]
    fn codes_have_the_standard_light_border_and_two_sizes() {
        let code = Code::new("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266").unwrap();
        assert_eq!(
            (code.width(Scale::Half), code.height(Scale::Half)),
            (37, 19)
        );
        assert_eq!(
            (code.width(Scale::Full), code.height(Scale::Full)),
            (74, 37)
        );
        assert!(!code.module(3, 3) && !code.module(0, 36));
        assert!(code.module(4, 4));
        assert_eq!(code.scale(80, 40), Some(Scale::Full));
        assert_eq!(code.scale(80, 30), Some(Scale::Half));
        assert_eq!(code.scale(30, 30), None);
    }
}
