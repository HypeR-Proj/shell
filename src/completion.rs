// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Filesystem and command completion never executes shell expansion or helpers.

use crate::interpreter::{BUILTINS, State};
use std::collections::BTreeSet;

pub struct Completion {
    pub start: usize,
    pub replacement: String,
    pub choices: Vec<String>,
}

#[derive(Clone, Copy)]
enum Entries {
    Files,
    Directories,
    All,
}

pub fn complete(line: &str, cursor: usize, state: &State) -> Option<Completion> {
    let before = line.get(..cursor)?;
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut command = true;
    let mut command_name = None;
    let mut redirect_target = false;
    for (i, c) in before.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if quote == Some(c) {
            quote = None;
            continue;
        }
        if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
            continue;
        }
        if quote.is_none() && (c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>')) {
            if i > start {
                let word = &before[start..i];
                let descriptor = matches!(c, '<' | '>') && word.bytes().all(|b| b.is_ascii_digit());
                if command && !redirect_target && !descriptor {
                    command_name = shlex::split(word).and_then(|words| words.into_iter().next());
                    command = false;
                }
                redirect_target = false;
            }
            if matches!(c, ';' | '|' | '&' | '\n') {
                command = true;
                command_name = None;
                redirect_target = false;
            } else if matches!(c, '<' | '>') {
                redirect_target = true;
            }
            start = i + c.len_utf8();
        }
    }
    let raw = &before[start..];
    // Completing a quoted word supplies its closing quote on a unique match.
    let input = if let Some(q) = quote {
        format!("{raw}{q}")
    } else {
        raw.to_owned()
    };
    let decoded = shlex::split(&input)?;
    let prefix = decoded.first().map_or("", String::as_str);
    let mut choices = BTreeSet::new();
    if command && !redirect_target && !prefix.contains('/') {
        choices.extend(
            BUILTINS
                .iter()
                .filter(|v| v.starts_with(prefix))
                .map(|v| (*v).to_owned()),
        );
        choices.extend(
            state
                .functions
                .keys()
                .filter(|v| v.starts_with(prefix))
                .cloned(),
        );
        for directory in state.get("PATH").unwrap_or("").split(':') {
            collect(
                &state.path(directory),
                prefix,
                "",
                Entries::Files,
                &mut choices,
            );
        }
    }
    let (directory, name) = prefix
        .rsplit_once('/')
        .map_or(("", prefix), |(d, n)| (&prefix[..d.len() + 1], n));
    collect(
        &state.path(if directory.is_empty() { "." } else { directory }),
        name,
        directory,
        if command_name.as_deref() == Some("cd") && !redirect_target {
            Entries::Directories
        } else {
            Entries::All
        },
        &mut choices,
    );
    if choices.is_empty() {
        return None;
    }
    let choices: Vec<_> = choices.into_iter().take(256).collect();
    let mut common = choices[0].clone();
    for choice in &choices[1..] {
        while !choice.starts_with(&common) {
            common.pop();
        }
    }
    let unique = choices.len() == 1;
    let replacement = if unique || common.len() > prefix.len() {
        let mut replacement = shlex::try_quote(&common).ok()?.into_owned();
        if unique && !common.ends_with('/') {
            replacement.push(' ');
        }
        replacement
    } else {
        raw.to_owned()
    };
    Some(Completion {
        start,
        replacement,
        // Candidate labels show the next path component. The replacement and
        // its common prefix keep the path the caller actually typed.
        choices: if unique {
            vec![]
        } else {
            choices
                .into_iter()
                .map(|choice| choice.strip_prefix(directory).unwrap_or(&choice).to_owned())
                .collect()
        },
    })
}

fn collect(
    directory: &str,
    prefix: &str,
    rendered: &str,
    kind: Entries,
    output: &mut BTreeSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.take(8192).flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(prefix) || name.starts_with('.') && !prefix.starts_with('.') {
            continue;
        }
        let directory = entry.file_type().is_ok_and(|t| {
            t.is_dir()
                || t.is_symlink() && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir())
        });
        if matches!(kind, Entries::Files) && directory
            || matches!(kind, Entries::Directories) && !directory
        {
            continue;
        }
        output.insert(format!(
            "{rendered}{name}{}",
            if directory { "/" } else { "" }
        ));
        if output.len() >= 256 {
            break;
        }
    }
}

#[cfg(test)]
#[path = "../tests/completion.rs"]
mod tests;
