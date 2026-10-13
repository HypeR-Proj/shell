// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

pub mod cli;
pub mod completion;
pub mod editor;
pub mod history;
pub mod interpreter;
pub mod layout;
pub mod path;
pub mod program;
pub mod syntax;

#[cfg(test)]
#[path = "../tests/interpreter.rs"]
mod interpreter_tests;
