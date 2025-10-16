use ratatui::{
    prelude::{Buffer, Rect},
    style::{Color, Style},
    widgets::Widget,
};

const GLYPH_HEIGHT: usize = 5;

pub struct Banner<'a> {
    text: &'a str,
    style: Style,
    spacing: u16,
}

impl<'a> Banner<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            style: Style::new().fg(Color::White),
            spacing: 1,
        }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn spacing(mut self, spacing: u16) -> Self {
        self.spacing = spacing;
        self
    }
}

impl<'a> Widget for Banner<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let chars: Vec<char> = self.text.chars().collect();
        let mut x_offset: u16 = 0;
        let mut y_offset: u16 = 0;
        let width = area.width;
        let height = area.height;

        for ch in chars {
            let mapped = match ch {
                ' ' => ' ',
                _ => ch.to_ascii_uppercase(),
            };
            let glyph = match glyph_for(mapped) {
                Some(glyph) => glyph,
                None => continue,
            };

            let glyph_width = glyph[0].len() as u16;
            if glyph_width == 0 {
                continue;
            }

            if x_offset + glyph_width > width {
                x_offset = 0;
                y_offset = y_offset.saturating_add(GLYPH_HEIGHT as u16 + 1);
            }

            if y_offset + GLYPH_HEIGHT as u16 > height {
                break;
            }

            for (row, line) in glyph.iter().enumerate() {
                let y = area.top() + y_offset + row as u16;
                if y >= area.bottom() {
                    continue;
                }
                for (col, symbol) in line.chars().enumerate() {
                    if symbol == ' ' {
                        continue;
                    }
                    let x = area.left() + x_offset + col as u16;
                    if x >= area.right() {
                        continue;
                    }
                    let cell = buf.get_mut(x, y);
                    cell.set_char(symbol);
                    cell.set_style(self.style);
                }
            }

            x_offset = x_offset.saturating_add(glyph_width);
            if x_offset < width {
                x_offset = x_offset.saturating_add(self.spacing).min(width);
            }
        }
    }
}

fn glyph_for(ch: char) -> Option<[&'static str; GLYPH_HEIGHT]> {
    match ch {
        'A' => Some([" █ ", "█ █", "███", "█ █", "█ █"]),
        'D' => Some(["██ ", "█ █", "█ █", "█ █", "██ "]),
        'E' => Some(["███", "█  ", "██ ", "█  ", "███"]),
        'G' => Some([" ██", "█  ", "███", "█ █", " ██"]),
        'I' => Some(["███", " █ ", " █ ", " █ ", "███"]),
        'L' => Some(["█  ", "█  ", "█  ", "█  ", "███"]),
        'N' => Some(["█ █", "███", "███", "█ █", "█ █"]),
        'O' => Some([" █ ", "█ █", "█ █", "█ █", " █ "]),
        'P' => Some(["██ ", "█ █", "██ ", "█  ", "█  "]),
        'R' => Some(["██ ", "█ █", "██ ", "█ █", "█ █"]),
        'S' => Some([" ██", "█  ", " ██", "  █", "██ "]),
        'T' => Some(["███", " █ ", " █ ", " █ ", " █ "]),
        'V' => Some(["█ █", "█ █", "█ █", "█ █", " █ "]),
        'W' => Some(["█ █", "█ █", "█ █", "███", "█ █"]),
        '<' => Some(["  █", " █ ", "█  ", " █ ", "  █"]),
        '>' => Some(["█  ", " █ ", "  █", " █ ", "█  "]),
        ' ' => Some(["   ", "   ", "   ", "   ", "   "]),
        _ => None,
    }
}
