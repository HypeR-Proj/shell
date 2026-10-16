// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Terminal rendering is separate from input editing and shell evaluation.

use crate::editor::Editor;
use crate::layout::Position;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn display(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\n' => out.push_str(" ↵ "),
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub struct Renderer {
    position: Position,
    columns: usize,
}
impl Default for Renderer {
    fn default() -> Self {
        Self {
            position: Position { row: 0, column: 10 },
            columns: 80,
        }
    }
}
impl Renderer {
    pub fn columns(&mut self, columns: u16) {
        self.columns = usize::from(columns).max(1);
    }
    pub fn reset(&mut self) {
        self.prompt("hyper-sh$ ");
    }
    pub fn prompt(&mut self, prompt: &str) {
        self.position = Position::default();
        self.position.append(prompt, self.columns);
    }
    pub fn track(&mut self, editor: &Editor, _prompt: &str) {
        // Echo is append-only. Inspect at most the last UTF-8 scalar instead
        // of rescanning an increasingly long line for every incoming byte.
        let end = editor.cursor();
        let mut start = end.saturating_sub(1);
        while start > 0 && editor.line()[start] & 0xc0 == 0x80 {
            start -= 1;
        }
        if let Ok(character) = std::str::from_utf8(&editor.line()[start..end]) {
            self.position.append(character, self.columns);
        }
    }
    /// Repaint only the suffix affected by deleting the previous scalar.
    /// Combining/format characters can change the preceding glyph, so those
    /// edits retain the complete redraw until editing is grapheme-aware.
    pub fn backspace(&mut self, editor: &Editor, prompt: &str, removed: char) -> String {
        let Ok(line) = std::str::from_utf8(editor.line()) else {
            return self.draw(editor, prompt);
        };
        let (prefix, suffix) = line.split_at(editor.cursor());
        // Regional indicators form flag glyphs even when their combined width
        // happens to equal the sum of the individual scalar widths.
        let requires_context =
            |c: char| c.width().unwrap_or(0) == 0 || ('\u{1f1e6}'..='\u{1f1ff}').contains(&c);
        if removed.is_control()
            || requires_context(removed)
            || prefix.chars().next_back().is_some_and(requires_context)
            || suffix.chars().next().is_some_and(requires_context)
            || !scalar_width_matches(line)
            || !scalar_width_matches(&format!("{prefix}{removed}{suffix}"))
        {
            return self.draw(editor, prompt);
        }
        let mut cursor = Position::default();
        cursor.append(prompt, self.columns);
        cursor.append(&display(prefix), self.columns);
        cursor.materialize_wrap(self.columns);
        let output = if suffix.is_empty()
            && self.position.row == cursor.row
            && self.position.column < self.columns
            && self.position.column > cursor.column
        {
            let cells = self.position.column - cursor.column;
            format!(
                "{}{}{}",
                "\x08".repeat(cells),
                " ".repeat(cells),
                "\x08".repeat(cells)
            )
        } else {
            // Returning to column zero also cancels the terminal's pending
            // wrap at the right margin. Clear from the edit point through any
            // old wrapped rows, leaving the prompt and prefix untouched.
            let mut output = cursor_move(self.position, cursor);
            output.push_str("\x1b[J");
            let suffix = display(suffix);
            output.push_str(&suffix);
            let mut end = cursor;
            end.append(&suffix, self.columns);
            if end.materialize_wrap(self.columns) {
                output.push_str("\r\n");
            }
            if !suffix.is_empty() {
                output.push_str(&cursor_move(end, cursor));
            }
            output
        };
        self.position = cursor;
        output
    }

    pub fn draw(&mut self, editor: &Editor, prompt: &str) -> String {
        let prompt = if let Some(query) = editor.search() {
            format!(
                "(reverse-search: {}) ",
                display(&String::from_utf8_lossy(query))
            )
        } else {
            prompt.into()
        };
        let line = display(&String::from_utf8_lossy(editor.line()));
        let prefix = display(&String::from_utf8_lossy(&editor.line()[..editor.cursor()]));
        let mut output = String::from("\r");
        if self.position.row != 0 {
            output.push_str(&format!("\x1b[{}A", self.position.row));
        }
        output.push_str("\x1b[J");
        output.push_str(&prompt);
        output.push_str(&line);
        let mut end = Position::default();
        end.append(&prompt, self.columns);
        end.append(&line, self.columns);
        if end.materialize_wrap(self.columns) {
            output.push_str("\r\n");
        }
        let mut cursor = Position::default();
        cursor.append(&prompt, self.columns);
        cursor.append(&prefix, self.columns);
        cursor.materialize_wrap(self.columns);
        self.position = cursor;
        output.push_str(&cursor_move(end, cursor));
        output
    }
}

fn cursor_move(from: Position, to: Position) -> String {
    let mut output = String::new();
    if from.row > to.row {
        output.push_str(&format!("\x1b[{}A", from.row - to.row));
    } else if from.row < to.row {
        output.push_str(&format!("\x1b[{}B", to.row - from.row));
    }
    // Use VT100 cursor controls: minicom interprets CSI n G (CHA) as a
    // return to column zero, ignoring n. CR also clears pending line wrap.
    output.push('\r');
    if to.column != 0 {
        output.push_str(&format!("\x1b[{}C", to.column));
    }
    output
}

fn scalar_width_matches(text: &str) -> bool {
    // Flags, emoji modifiers and ligatures need more than per-scalar cell
    // arithmetic. Avoid erasing neighboring glyphs with the local fast path.
    text.width() == text.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>()
}

#[cfg(test)]
#[path = "../tests/interactive.rs"]
mod tests;
