// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Shell syntax retains quoting until expansion, rather than tokenizing with shlex.

mod lexer;
mod parser;

use serde::{Deserialize, Serialize};

pub const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
pub const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Script(pub Vec<AndOr>);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(bool, Pipeline)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pipeline {
    pub negate: bool,
    pub commands: Vec<Command>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Command {
    pub kind: CommandKind,
    pub redirects: Vec<Redirect>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CommandKind {
    Simple(Vec<String>),
    Group(Script),
    Subshell(Script),
    If {
        branches: Vec<(Script, Script)>,
        otherwise: Script,
    },
    While {
        until: bool,
        condition: Script,
        body: Script,
    },
    For {
        name: String,
        words: Option<Vec<String>>,
        body: Script,
    },
    Case {
        word: String,
        arms: Vec<(Vec<String>, Script)>,
    },
    Function {
        name: String,
        body: Box<Command>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Redirect {
    pub fd: u8,
    pub op: String,
    pub word: String,
    pub here: Option<HereDocument>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HereDocument {
    pub body: String,
    pub expand: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub message: String,
    pub incomplete: bool,
}

impl Error {
    fn new(offset: usize, message: impl Into<String>, incomplete: bool) -> Self {
        Self {
            offset,
            message: message.into(),
            incomplete,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (byte {})", self.message, self.offset + 1)
    }
}

impl std::error::Error for Error {}

pub fn parse(source: &str) -> Result<Script, Error> {
    if source.len() > MAX_SCRIPT_BYTES || source.contains('\0') {
        return Err(Error::new(0, "script is too large or contains NUL", false));
    }
    parser::parse(lexer::lex(source)?)
}

pub fn identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
