// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Portable shell evaluation. Native process creation stays in the application.

mod arithmetic;
mod builtin;
mod evaluate;
mod expand;
mod format;
pub mod io;
pub mod pattern;
mod read;
mod state;

use crate::syntax::{Command, Redirect, Script};
pub use evaluate::Shell;
use io::Streams;
use serde::{Deserialize, Serialize};
pub use state::{State, Variable};

pub type Result<T> = std::result::Result<T, String>;

/// An external image or an isolated shell stage. All stages start before the
/// launcher waits, including functions and compound commands in pipelines.
pub enum Invocation {
    External {
        arguments: Vec<String>,
        environment: Vec<String>,
    },
    Shell(Box<Worker>),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Worker {
    pub state: State,
    pub command: Command,
    #[serde(default)]
    pub descriptors: Vec<u8>,
    #[serde(default)]
    pub aliases: Vec<(u8, u8)>,
    #[serde(default)]
    pub history: Vec<String>,
    #[serde(default)]
    pub call_depth: usize,
    #[serde(default)]
    pub function_scope: bool,
}

pub struct Stage {
    pub invocation: Invocation,
    pub streams: Streams,
}

pub trait Backend {
    fn pipeline(&mut self, stages: Vec<Stage>) -> Result<Vec<i32>>;
    fn directory(&mut self, path: &str) -> Result<()>;
    fn canonical_input(&mut self, _enabled: bool) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    None,
    Exit(i32),
    Return(i32),
    Break(usize),
    Continue(usize),
}

pub const BUILTINS: &[&str] = &[
    ":", ".", "[", "break", "cd", "clear", "continue", "echo", "eval", "exec", "exit", "export",
    "false", "help", "history", "local", "printf", "pwd", "read", "readonly", "return", "set",
    "shift", "source", "status", "test", "true", "type", "unset",
];

fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    crate::syntax::identifier(name).then_some((name, value))
}

fn absolute(cwd: &str, path: &str) -> String {
    let joined = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{cwd}/{path}")
    };
    let mut parts = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}
