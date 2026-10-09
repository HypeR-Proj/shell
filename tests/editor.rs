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
    for index in 0..40 {
        type_bytes(&mut editor, format!("echo {index}\n").as_bytes());
    }
    assert_eq!(editor.history.len(), HISTORY_LINES);
    assert_eq!(
        editor.history.front().map(Vec::as_slice),
        Some(b"echo 8".as_slice())
    );
    type_bytes(&mut editor, b"echo 39\n secret\n\n");
    assert_eq!(editor.history.len(), HISTORY_LINES);
    type_bytes(&mut editor, &vec![b'x'; MAX_LINE_BYTES + 1]);
    assert!(matches!(editor.push(b'\n'), Action::TooLong));
    assert_eq!(
        editor.history.back().map(Vec::as_slice),
        Some(b"echo 39".as_slice())
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
