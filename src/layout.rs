// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Cursor layout for printable terminal text, including pending autowrap and
//! the unused final cell when a wide character wraps to the next row.

use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Default)]
pub struct Position {
    pub row: usize,
    pub column: usize,
}

impl Position {
    pub fn append(&mut self, text: &str, columns: usize) {
        let columns = columns.max(1);
        for c in text.chars() {
            let width = c.width().unwrap_or(0).min(columns);
            if width == 0 {
                continue;
            }
            if self.column + width > columns {
                self.row += 1;
                self.column = 0;
            }
            self.column += width;
        }
    }

    pub fn materialize_wrap(&mut self, columns: usize) -> bool {
        if self.column == columns.max(1) {
            self.row += 1;
            self.column = 0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Position;

    #[test]
    fn wide_characters_and_combining_marks_preserve_wrap_position() {
        let mut p = Position::default();
        p.append("1234567界", 8);
        assert_eq!((p.row, p.column), (1, 2));
        p.append("123456\u{301}", 8);
        assert_eq!((p.row, p.column), (1, 8));
        assert!(p.materialize_wrap(8));
        assert_eq!((p.row, p.column), (2, 0));
        assert!(!p.materialize_wrap(8));
    }
}
