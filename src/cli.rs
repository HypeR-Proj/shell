// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(about = "Native capability-scoped command shell")]
pub struct Shell {}

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
    #[command(disable_help_flag = true)]
    Echo(EchoWords),
}

#[derive(Debug, Args)]
pub struct Cd {
    #[arg(default_value = "/")]
    pub directory: String,
}

#[derive(Debug, Args)]
pub struct EchoWords {
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub words: Vec<String>,
}

#[cfg(test)]
#[path = "../tests/cli.rs"]
mod tests;
