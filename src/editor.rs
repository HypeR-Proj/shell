// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! UTF-8 line editing; terminal policy and process ownership remain external.

pub const MAX_LINE_BYTES: usize = 4096;
use std::collections::VecDeque;

const HISTORY_LINES: usize = crate::history::MAX_ENTRIES;

pub enum Action {
    None,
    Echo(u8),
    Redraw,
    ClearScreen,
    Complete,
    Submit(Vec<u8>),
    TooLong,
    Cancel,
    Exit,
}

#[derive(Default)]
enum Escape {
    #[default]
    None,
    Start,
    Sequence(Vec<u8>),
}

#[derive(Default)]
pub struct Editor {
    line: Vec<u8>,
    cursor: usize,
    history: VecDeque<Vec<u8>>,
    selected: Option<usize>,
    draft: Vec<u8>,
    escape: Escape,
    overflow: bool,
    carriage_return: bool,
    paste: bool,
    kill: Vec<u8>,
    search: Option<Vec<u8>>,
}

impl Editor {
    pub fn line(&self) -> &[u8] {
        &self.line
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn search(&self) -> Option<&[u8]> {
        self.search.as_deref()
    }
    pub fn set_history(&mut self, entries: &[String]) {
        self.history = entries
            .iter()
            .rev()
            .take(HISTORY_LINES)
            .rev()
            .map(|s| s.as_bytes().to_vec())
            .collect();
        self.selected = None;
    }
    pub fn replace(&mut self, start: usize, text: &str) {
        if start <= self.cursor
            && self.line.len() - (self.cursor - start) + text.len() <= MAX_LINE_BYTES
        {
            self.line.splice(start..self.cursor, text.bytes());
            self.cursor = start + text.len();
        }
    }
    pub fn push(&mut self, byte: u8) -> Action {
        if byte == 27 {
            self.escape = Escape::Start;
            return Action::None;
        }
        match std::mem::take(&mut self.escape) {
            Escape::Start => {
                if matches!(byte, b'[' | b'O') {
                    self.escape = Escape::Sequence(Vec::new());
                }
                return Action::None;
            }
            Escape::Sequence(mut sequence) => {
                if (0x40..=0x7e).contains(&byte) {
                    return self.sequence(&sequence, byte);
                }
                if sequence.len() < 16 && (0x20..=0x3f).contains(&byte) {
                    sequence.push(byte);
                    self.escape = Escape::Sequence(sequence);
                }
                return Action::None;
            }
            Escape::None => {}
        }
        if self.paste {
            if byte == b'\n' && self.carriage_return {
                self.carriage_return = false;
                return Action::None;
            }
            self.carriage_return = byte == b'\r';
            return match byte {
                b'\r' => self.insert(b'\n'),
                b'\n' | b'\t' => self.insert(byte),
                0x20..=0xff if byte != 127 => self.insert(byte),
                _ => Action::None,
            };
        }
        if byte == b'\n' && self.carriage_return {
            self.carriage_return = false;
            return Action::None;
        }
        self.carriage_return = byte == b'\r';
        if self.search.is_some() {
            match byte {
                18 => return self.search_older(),
                b'\r' | b'\n' => {
                    self.search = None;
                    return Action::Redraw;
                }
                7 | 3 => {
                    self.line = self.draft.clone();
                    self.cursor = self.line.len();
                    self.search = None;
                    return Action::Redraw;
                }
                8 | 127 => {
                    if let Some(query) = self.search.as_mut() {
                        pop_character(query);
                    }
                    self.selected = None;
                    return self.search_older();
                }
                b' '..=b'~' | 0x80..=0xff => {
                    if let Some(query) = self.search.as_mut() {
                        query.push(byte);
                    }
                    self.selected = None;
                    return self.search_older();
                }
                _ => return Action::None,
            }
        }
        match byte {
            b'\r' | b'\n' => self.submit(),
            3 => {
                self.reset();
                Action::Cancel
            }
            4 if self.line.is_empty() => Action::Exit,
            4 => self.delete(),
            1 => {
                self.cursor = 0;
                Action::Redraw
            }
            5 => {
                self.cursor = self.line.len();
                Action::Redraw
            }
            2 => {
                self.cursor = previous(&self.line, self.cursor);
                Action::Redraw
            }
            6 => {
                self.cursor = next(&self.line, self.cursor);
                Action::Redraw
            }
            11 => {
                self.kill = self.line.drain(self.cursor..).collect();
                Action::Redraw
            }
            21 => {
                self.kill = self.line.drain(..self.cursor).collect();
                self.cursor = 0;
                self.overflow = false;
                Action::Redraw
            }
            23 => {
                let end = self.cursor;
                while self.cursor != 0 && self.line[self.cursor - 1].is_ascii_whitespace() {
                    self.cursor = previous(&self.line, self.cursor);
                }
                while self.cursor != 0 && !self.line[self.cursor - 1].is_ascii_whitespace() {
                    self.cursor = previous(&self.line, self.cursor);
                }
                self.kill = self.line.drain(self.cursor..end).collect();
                Action::Redraw
            }
            25 => {
                if self.line.len() + self.kill.len() <= MAX_LINE_BYTES {
                    self.line
                        .splice(self.cursor..self.cursor, self.kill.iter().copied());
                    self.cursor += self.kill.len();
                }
                Action::Redraw
            }
            12 => Action::ClearScreen,
            16 => self.recall(true),
            14 => self.recall(false),
            18 => {
                self.draft = self.line.clone();
                self.search = Some(Vec::new());
                self.selected = None;
                self.search_older()
            }
            b'\t' => Action::Complete,
            8 | 127 if !self.overflow => {
                if self.cursor == 0 {
                    return Action::None;
                }
                let start = previous(&self.line, self.cursor);
                self.line.drain(start..self.cursor);
                self.cursor = start;
                Action::Redraw
            }
            b' '..=b'~' | 0x80..=0xff => self.insert(byte),
            _ => Action::None,
        }
    }
    fn insert(&mut self, byte: u8) -> Action {
        if self.overflow {
            return Action::None;
        }
        if self.line.len() >= MAX_LINE_BYTES {
            self.overflow = true;
            return Action::None;
        }
        let end = self.cursor == self.line.len();
        self.line.insert(self.cursor, byte);
        self.cursor += 1;
        if self.paste {
            // Render once when the bracketed paste closes, not once per byte.
            Action::None
        } else if end {
            Action::Echo(byte)
        } else if std::str::from_utf8(&self.line).is_ok() {
            Action::Redraw
        } else {
            Action::None
        }
    }
    fn delete(&mut self) -> Action {
        let end = next(&self.line, self.cursor);
        self.line.drain(self.cursor..end);
        Action::Redraw
    }
    fn sequence(&mut self, sequence: &[u8], end: u8) -> Action {
        if end == b'~' {
            match sequence {
                b"200" => {
                    self.paste = true;
                    self.carriage_return = false;
                    return Action::None;
                }
                b"201" => {
                    self.paste = false;
                    return Action::Redraw;
                }
                b"3" if !self.paste => return self.delete(),
                b"1" | b"7" if !self.paste => {
                    self.cursor = 0;
                    return Action::Redraw;
                }
                b"4" | b"8" if !self.paste => {
                    self.cursor = self.line.len();
                    return Action::Redraw;
                }
                _ => return Action::None,
            }
        }
        if !sequence.is_empty() || self.paste {
            return Action::None;
        }
        match end {
            b'A' => self.recall(true),
            b'B' => self.recall(false),
            b'C' => {
                self.cursor = next(&self.line, self.cursor);
                Action::Redraw
            }
            b'D' => {
                self.cursor = previous(&self.line, self.cursor);
                Action::Redraw
            }
            b'H' => {
                self.cursor = 0;
                Action::Redraw
            }
            b'F' => {
                self.cursor = self.line.len();
                Action::Redraw
            }
            _ => Action::None,
        }
    }
    fn submit(&mut self) -> Action {
        let overflow = self.overflow;
        let line = std::mem::take(&mut self.line);
        self.reset();
        if overflow {
            return Action::TooLong;
        }
        if line.first().is_some_and(|byte| !byte.is_ascii_whitespace())
            && self.history.back() != Some(&line)
        {
            if self.history.len() == HISTORY_LINES {
                self.history.pop_front();
            }
            self.history.push_back(line.clone());
        }
        Action::Submit(line)
    }
    fn reset(&mut self) {
        self.line.clear();
        self.cursor = 0;
        self.draft.clear();
        self.selected = None;
        self.overflow = false;
        self.escape = Escape::None;
        self.search = None;
        self.paste = false;
    }
    fn recall(&mut self, older: bool) -> Action {
        if self.overflow || self.history.is_empty() {
            return Action::None;
        }
        if older {
            match self.selected {
                None => {
                    self.draft = self.line.clone();
                    self.selected = Some(self.history.len() - 1);
                }
                Some(0) => return Action::None,
                Some(index) => self.selected = Some(index - 1),
            }
        } else {
            match self.selected {
                None => return Action::None,
                Some(index) if index + 1 == self.history.len() => self.selected = None,
                Some(index) => self.selected = Some(index + 1),
            }
        }
        self.line = self
            .selected
            .map_or_else(|| self.draft.clone(), |index| self.history[index].clone());
        self.cursor = self.line.len();
        Action::Redraw
    }
    fn search_older(&mut self) -> Action {
        let Some(query) = self.search.as_ref() else {
            return Action::None;
        };
        let end = self.selected.unwrap_or(self.history.len());
        if let Some(index) = (0..end)
            .rev()
            .find(|i| query.is_empty() || self.history[*i].windows(query.len()).any(|w| w == query))
        {
            self.selected = Some(index);
            self.line = self.history[index].clone();
            self.cursor = self.line.len();
        }
        Action::Redraw
    }
}

fn previous(line: &[u8], cursor: usize) -> usize {
    let mut cursor = cursor.saturating_sub(1);
    while cursor > 0 && line[cursor] & 0xc0 == 0x80 {
        cursor -= 1;
    }
    cursor
}
fn next(line: &[u8], cursor: usize) -> usize {
    let mut cursor = (cursor + 1).min(line.len());
    while cursor < line.len() && line[cursor] & 0xc0 == 0x80 {
        cursor += 1;
    }
    cursor
}
fn pop_character(bytes: &mut Vec<u8>) {
    let at = previous(bytes, bytes.len());
    bytes.truncate(at);
}

#[cfg(test)]
#[path = "../tests/editor.rs"]
mod tests;
