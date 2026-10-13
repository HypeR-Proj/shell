// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn command_script_and_positional_arguments() -> Result<(), clap::Error> {
    let args = Shell::try_parse_from(["sh", "-c", "echo $1", "command", "-x"])?;
    assert_eq!(args.command.as_deref(), Some("echo $1"));
    assert_eq!(args.arguments, ["command", "-x"]);
    let args = Shell::try_parse_from(["sh", "-n", "script.sh", "--arg"])?;
    assert!(args.check);
    assert_eq!(args.arguments, ["script.sh", "--arg"]);
    Ok(())
}
