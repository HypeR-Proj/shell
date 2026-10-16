// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::editor::Action;

struct Terminal {
    editor: Editor,
    renderer: Renderer,
    prompt: &'static str,
}

impl Terminal {
    fn new(prompt: &'static str, columns: u16) -> Self {
        let mut renderer = Renderer::default();
        renderer.columns(columns);
        renderer.prompt(prompt);
        Self {
            editor: Editor::default(),
            renderer,
            prompt,
        }
    }

    fn input(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        for &byte in bytes {
            match self.editor.push(byte) {
                Action::Echo(byte) => {
                    output.push(byte);
                    self.renderer.track(&self.editor, self.prompt);
                }
                Action::Backspace(removed) => output.extend_from_slice(
                    self.renderer
                        .backspace(&self.editor, self.prompt, removed)
                        .as_bytes(),
                ),
                Action::Redraw => output
                    .extend_from_slice(self.renderer.draw(&self.editor, self.prompt).as_bytes()),
                _ => {}
            }
        }
        output
    }

    fn position(&self) -> (usize, usize) {
        (self.renderer.position.row, self.renderer.position.column)
    }
}

#[test]
fn tail_backspace_erases_cells_without_reprinting_prompt_or_prefix() {
    for key in [8, 127] {
        let mut terminal = Terminal::new("hyper-sh$ ", 80);
        terminal.input(b"abc");
        assert_eq!(terminal.input(&[key]), b"\x08 \x08");
        assert_eq!(terminal.editor.line(), b"ab");
        assert_eq!(terminal.position(), (0, 12));
        terminal.input("界".as_bytes());
        assert_eq!(terminal.input(&[key]), b"\x08\x08  \x08\x08");
        assert_eq!(terminal.editor.line(), b"ab");
        assert_eq!(terminal.position(), (0, 12));
    }
}

#[test]
fn middle_backspace_repaints_only_the_shifted_suffix() {
    let mut terminal = Terminal::new("hyper-sh$ ", 80);
    terminal.input(b"abcdef\x1b[D\x1b[D\x1b[D");
    assert_eq!(terminal.input(&[127]), b"\r\x1b[12C\x1b[Jdef\r\x1b[12C");
    assert_eq!(terminal.editor.line(), b"abdef");
    assert_eq!(terminal.position(), (0, 12));
    terminal.input(b"\x01");
    assert!(terminal.input(&[127]).is_empty());
}

#[test]
fn erase_at_pending_right_margin_cancels_wrap_before_clearing() {
    let mut terminal = Terminal::new("> ", 8);
    terminal.input(b"abcdef");
    assert_eq!(terminal.position(), (0, 8));
    assert_eq!(terminal.input(&[127]), b"\r\x1b[7C\x1b[J");
    assert_eq!(terminal.position(), (0, 7));
    terminal.input(b"XY");
    assert_eq!(terminal.position(), (1, 1));
    assert_eq!(terminal.input(&[127]), b"\x08 \x08");
    assert_eq!(terminal.position(), (1, 0));
    assert_eq!(terminal.input(&[127]), b"\x1b[1A\r\x1b[7C\x1b[J");
    assert_eq!(terminal.position(), (0, 7));
}

#[test]
fn wide_character_wrap_erases_the_old_row_without_reprinting_prefix() {
    let mut terminal = Terminal::new("> ", 8);
    terminal.input("abcde界".as_bytes());
    assert_eq!(terminal.position(), (1, 2));
    assert_eq!(terminal.input(&[127]), b"\x1b[1A\r\x1b[7C\x1b[J");
    assert_eq!(terminal.editor.line(), b"abcde");
    assert_eq!(terminal.position(), (0, 7));
}

#[test]
fn middle_erase_can_pull_suffix_back_across_a_wrap() {
    let mut terminal = Terminal::new("> ", 8);
    terminal.input(b"abcdeXyz\x1b[D\x1b[D");
    assert_eq!(terminal.position(), (1, 0));
    assert_eq!(
        terminal.input(&[127]),
        b"\x1b[1A\r\x1b[7C\x1b[Jyz\x1b[1A\r\x1b[7C"
    );
    assert_eq!(terminal.editor.line(), b"abcdeyz");
    assert_eq!(terminal.position(), (0, 7));
}

#[test]
fn combining_or_incomplete_utf8_retains_safe_redraw() {
    let mut terminal = Terminal::new("> ", 80);
    terminal.input("e\u{301}".as_bytes());
    assert_eq!(terminal.input(&[127]), b"\r\x1b[J> e\r\x1b[3C");
    assert_eq!(terminal.position(), (0, 3));
    terminal.input(&[0xe4]);
    assert_eq!(terminal.input(&[127]), b"\r\x1b[J> e\r\x1b[3C");
    terminal.input("a\u{301}b\x1b[D\x1b[D".as_bytes());
    // Removing the base before a combining mark must redraw the preceding glyph.
    let output = terminal.input(&[127]);
    assert!(output.starts_with(b"\r\x1b[J> "));
}

#[test]
fn composite_emoji_does_not_erase_the_surviving_base_glyph() {
    for text in ["\u{1f44d}\u{1f3fb}", "\u{1f1e8}\u{1f1f3}"] {
        let mut terminal = Terminal::new("> ", 80);
        terminal.input(text.as_bytes());
        assert!(terminal.input(&[127]).starts_with(b"\r\x1b[J> "));
    }
}
