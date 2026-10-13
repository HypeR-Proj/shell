// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;

#[derive(Debug, Parser)]
#[command(about = "Native system shell", trailing_var_arg = true)]
pub struct Shell {
    /// Execute a command string; following words supply $0, $1, ... .
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub command: Option<String>,
    /// Read commands interactively.
    #[arg(short = 'i')]
    pub interactive: bool,
    /// Parse a script without executing it.
    #[arg(short = 'n')]
    pub check: bool,
    #[arg(long, hide = true)]
    pub worker: bool,
    /// Script path and positional arguments (or command-string arguments).
    pub arguments: Vec<String>,
}

#[cfg(test)]
#[path = "../tests/cli.rs"]
mod tests;
