// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Terminal rendering is separate from input editing and shell evaluation.

use crate::{Error, write_terminal};
use hyper_shell::editor::Editor;
use hyper_shell::layout::Position;

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
    pub fn draw(&mut self, editor: &Editor, prompt: &str) -> Result<(), Error> {
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
        write_terminal(b"\r")?;
        if self.position.row != 0 {
            write_terminal(format!("\x1b[{}A", self.position.row).as_bytes())?;
        }
        write_terminal(b"\x1b[J")?;
        write_terminal(prompt.as_bytes())?;
        write_terminal(line.as_bytes())?;
        let mut end = Position::default();
        end.append(&prompt, self.columns);
        end.append(&line, self.columns);
        if end.materialize_wrap(self.columns) {
            write_terminal(b"\r\n")?;
        }
        let mut cursor = Position::default();
        cursor.append(&prompt, self.columns);
        cursor.append(&prefix, self.columns);
        cursor.materialize_wrap(self.columns);
        self.position = cursor;
        if end.row > cursor.row {
            write_terminal(format!("\x1b[{}A", end.row - cursor.row).as_bytes())?;
        }
        write_terminal(format!("\x1b[{}G", cursor.column + 1).as_bytes())
    }
}
