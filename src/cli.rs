// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(about = "Native capability-scoped command shell")]
pub struct Shell {
    /// Execute an output-only builtin in a pipeline child.
    #[arg(long, hide = true, num_args = 1.., allow_hyphen_values = true)]
    pub builtin: Option<Vec<String>>,
}

#[derive(Debug, Parser)]
#[command(disable_help_subcommand = true)]
pub struct Builtin {
    #[command(subcommand)]
    pub command: BuiltinCommand,
}

#[derive(Debug, Subcommand)]
pub enum BuiltinCommand {
    Cd(Cd),
    Pwd,
    Clear,
    Exit,
    Help,
}

#[derive(Debug, Args)]
pub struct Cd {
    #[arg(default_value = "/")]
    pub directory: String,
}

#[cfg(test)]
#[path = "../tests/cli.rs"]
mod tests;
