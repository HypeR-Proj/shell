// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn type_bytes(editor: &mut Editor, bytes: &[u8]) {
    for byte in bytes {
        editor.push(*byte);
    }
}

#[test]
fn recall_restores_draft_and_does_not_mutate_saved_commands() {
    let mut editor = Editor::default();
    type_bytes(&mut editor, b"vmm start alpine\nvmm stop alpine\ndraft");
    type_bytes(&mut editor, b"\x1b[A");
    assert_eq!(editor.line(), b"vmm stop alpine");
    type_bytes(&mut editor, b" changed\x1b[A");
    assert_eq!(editor.line(), b"vmm start alpine");
    type_bytes(&mut editor, b"\x1b[B");
    assert_eq!(editor.line(), b"vmm stop alpine");
    type_bytes(&mut editor, b"\x1b[B");
    assert_eq!(editor.line(), b"draft");
    // A fresh escape restarts a truncated sequence instead of inserting '[A'.
    type_bytes(&mut editor, b"\x1b[\x1b[A");
    assert_eq!(editor.line(), b"vmm stop alpine");
}

#[test]
fn history_is_bounded_and_omits_private_duplicate_and_overlong_lines() {
    let mut editor = Editor::default();
    for index in 0..HISTORY_LINES + 8 {
        type_bytes(&mut editor, format!("echo {index}\n").as_bytes());
    }
    assert_eq!(editor.history.len(), HISTORY_LINES);
    assert_eq!(
        editor.history.front().map(Vec::as_slice),
        Some(b"echo 8".as_slice())
    );
    type_bytes(
        &mut editor,
        format!("echo {}\n secret\n\n", HISTORY_LINES + 7).as_bytes(),
    );
    assert_eq!(editor.history.len(), HISTORY_LINES);
    type_bytes(&mut editor, &vec![b'x'; MAX_LINE_BYTES + 1]);
    assert!(matches!(editor.push(b'\n'), Action::TooLong));
    assert_eq!(
        editor.history.back().map(Vec::as_slice),
        Some(format!("echo {}", HISTORY_LINES + 7).as_bytes())
    );
}

#[test]
fn split_escape_crlf_cancel_and_utf8_backspace() {
    let mut editor = Editor::default();
    type_bytes(&mut editor, b"echo ok");
    assert!(matches!(editor.push(b'\r'), Action::Submit(_)));
    assert!(matches!(editor.push(b'\n'), Action::None));
    type_bytes(&mut editor, b"\x1b[");
    assert!(matches!(editor.push(b'A'), Action::Redraw));
    assert_eq!(editor.line(), b"echo ok");
    assert!(matches!(editor.push(3), Action::Cancel));
    type_bytes(&mut editor, "a中".as_bytes());
    editor.push(127);
    assert_eq!(editor.line(), b"a");
    type_bytes(&mut editor, b"\x1b[1;5A");
    assert_eq!(editor.line(), b"a");
    editor.push(21);
    assert!(editor.line().is_empty());
    assert!(matches!(editor.push(4), Action::Exit));
}

#[test]
fn unicode_cursor_kill_yank_and_paste_do_not_execute() {
    let mut e = Editor::default();
    type_bytes(&mut e, "a中b".as_bytes());
    type_bytes(&mut e, b"\x1b[D\x7f");
    assert_eq!(e.line(), b"ab");
    type_bytes(&mut e, b"\x01\x06\x0b\x19");
    assert_eq!(e.line(), b"ab");
    e.push(3);
    for b in b"\x1b[200~echo a\r\necho b\x1b[201~" {
        assert!(!matches!(e.push(*b), Action::Submit(_)));
    }
    assert_eq!(e.line(), b"echo a\necho b");
    assert!(matches!(e.push(b'\r'), Action::Submit(_)));
}

#[test]
fn reverse_search_accepts_or_restores_the_draft() {
    let mut e = Editor::default();
    e.set_history(&["echo one".into(), "echo two".into()]);
    type_bytes(&mut e, b"draft\x12one");
    assert_eq!(e.line(), b"echo one");
    e.push(7);
    assert_eq!(e.line(), b"draft");
    type_bytes(&mut e, b"\x12two\r");
    assert_eq!(e.line(), b"echo two");
    assert!(matches!(e.push(b'\r'), Action::Submit(_)));
}
