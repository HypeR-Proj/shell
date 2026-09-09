// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn builtin_errors_and_help_return_to_shell() {
    assert!(Builtin::try_parse_from(["sh", "cd"]).is_ok());
    assert!(Builtin::try_parse_from(["sh", "pwd", "extra"]).is_err());
    assert!(
        Builtin::try_parse_from(["sh", "cd", "--help"])
            .is_err_and(|e| e.kind() == clap::error::ErrorKind::DisplayHelp)
    );
}
