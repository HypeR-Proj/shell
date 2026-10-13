// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Builtins execute in the shell, so state changes also work with redirection.

use super::format::{echo, printf};
use super::*;
use std::io::Read;

impl<B: Backend> Shell<B> {
    pub(super) fn builtin(&mut self, words: &[String], io: &Streams) -> Result<i32> {
        let args = &words[1..];
        match words[0].as_str() {
            ":" | "true" => Ok(0),
            "false" => Ok(1),
            "status" => {
                io.write(
                    1,
                    &format!(
                        "exit={} elapsed={:.3} ms pipeline={}\n",
                        self.state.status,
                        self.state.last_duration_ns as f64 / 1_000_000.0,
                        self.state
                            .pipeline_status
                            .iter()
                            .map(i32::to_string)
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                )?;
                Ok(0)
            }
            "pwd" => {
                io.write(1, &format!("{}\n", self.state.cwd))?;
                Ok(0)
            }
            "clear" => {
                io.write(1, "\x1b[2J\x1b[H")?;
                Ok(0)
            }
            "help" => {
                io.write(1, "Native system shell\n\nCommands: pipelines, ;, &&, ||, !, if/elif/else, for, while, until, case, functions.\nExpansion: variables, positional arguments, $(commands), $((arithmetic)), wildcards.\nRedirection: < > >> <> >|, n>&m n<&m n>&-, << <<- <<<, &> &>>.\nBuiltins: cd pwd echo printf read test [ export readonly unset local set shift\n          source . eval exec return break continue exit true false history type help.\nEditing: arrows, Home/End, Delete, Ctrl-A/E/U/K/W/Y, Ctrl-R, Tab, Ctrl-L.\nStatus: $? is the last result; ${PIPESTATUS[@]} lists pipeline results.\nJobs and Linux signal semantics are not implemented.\n")?;
                Ok(0)
            }
            "cd" => self.cd(args, io),
            "echo" => echo(args, io),
            "printf" => {
                printf(args, io)?;
                Ok(0)
            }
            "exit" | "return" => {
                if args.len() > 1 {
                    return Err("too many status arguments".into());
                }
                let code = args
                    .first()
                    .map(|s| s.parse::<i64>().map(|n| (n & 255) as i32))
                    .transpose()
                    .map_err(|_| "invalid exit status")?
                    .unwrap_or(self.state.status);
                if words[0] == "exit" {
                    self.flow = Flow::Exit(code);
                } else if self.call_depth != 0 {
                    self.flow = Flow::Return(code);
                } else {
                    return Err("return: outside a function or sourced script".into());
                }
                Ok(code)
            }
            "break" | "continue" => {
                let count = args
                    .first()
                    .map(|s| s.parse::<usize>())
                    .transpose()
                    .map_err(|_| "invalid loop count")?
                    .unwrap_or(1);
                if args.len() > 1 || count == 0 || self.loop_depth == 0 {
                    return Err("break/continue: not inside a loop or invalid count".into());
                }
                let count = count.min(self.loop_depth);
                self.flow = if words[0] == "break" {
                    Flow::Break(count)
                } else {
                    Flow::Continue(count)
                };
                Ok(0)
            }
            "export" | "readonly" | "local" => self.declare(&words[0], args, io),
            "unset" => {
                let functions = args.first().is_some_and(|s| s == "-f");
                for name in &args[usize::from(functions)..] {
                    if functions {
                        self.state.functions.remove(name);
                    } else {
                        if self.state.variables.get(name).is_some_and(|v| v.readonly) {
                            return Err(format!("{name}: readonly variable"));
                        }
                        self.state.variables.remove(name);
                    }
                }
                Ok(0)
            }
            "shift" => {
                let count = args
                    .first()
                    .map(|s| s.parse::<usize>())
                    .transpose()
                    .map_err(|_| "shift: invalid count")?
                    .unwrap_or(1);
                if args.len() > 1 || count > self.state.positional.len() {
                    return Err("shift: count exceeds positional arguments".into());
                }
                self.state.positional.drain(..count);
                Ok(0)
            }
            "set" => self.set_options(args, io),
            "read" => self.read(args, io),
            "test" | "[" => {
                let args = if words[0] == "[" {
                    if args.last().map(String::as_str) != Some("]") {
                        return Err("[: missing ]".into());
                    }
                    &args[..args.len() - 1]
                } else {
                    args
                };
                Ok(i32::from(!test(args, &self.state)?))
            }
            "source" | "." => {
                let path = args.first().ok_or("source: expected a path")?;
                let source = read_script(&self.state.path(path))?;
                if self.call_depth == 64 {
                    return Err("source nesting limit exceeded".into());
                }
                let saved_args = self.state.positional.clone();
                if args.len() > 1 {
                    self.state.positional = args[1..].to_vec();
                }
                self.call_depth += 1;
                let result = syntax_eval(self, &source, io);
                self.call_depth -= 1;
                if args.len() > 1 {
                    self.state.positional = saved_args;
                }
                if let Flow::Return(code) = self.flow {
                    self.flow = Flow::None;
                    Ok(code)
                } else {
                    result
                }
            }
            "eval" => {
                if self.call_depth == 64 {
                    return Err("eval nesting limit exceeded".into());
                }
                self.call_depth += 1;
                let result = syntax_eval(self, &args.join(" "), io);
                self.call_depth -= 1;
                result
            }
            "exec" => {
                if !args.is_empty() {
                    return Err("exec: image replacement is not supported; use a command or exit explicitly".into());
                }
                self.streams = io.clone();
                *self
                    .redirection_scopes
                    .last_mut()
                    .ok_or("exec outside command scope")? = true;
                self.io_revision = self.io_revision.wrapping_add(1);
                Ok(0)
            }
            "history" => {
                if args.first().is_some_and(|s| s == "-c") {
                    self.history.clear();
                } else {
                    for (i, entry) in self.history.iter().enumerate() {
                        io.write(1, &format!("{:5}  {entry}\n", i + 1))?;
                    }
                }
                Ok(0)
            }
            "type" => {
                let mut status = 0;
                for name in args {
                    if self.state.functions.contains_key(name) {
                        io.write(1, &format!("{name} is a shell function\n"))?;
                    } else if BUILTINS.contains(&name.as_str()) {
                        io.write(1, &format!("{name} is a shell builtin\n"))?;
                    } else if let Ok(path) = self.state.executable(name) {
                        io.write(1, &format!("{name} is {path}\n"))?;
                    } else {
                        io.write(2, &format!("{name}: not found\n"))?;
                        status = 1;
                    }
                }
                Ok(status)
            }
            _ => Err("unknown builtin".into()),
        }
    }
    fn cd(&mut self, args: &[String], io: &Streams) -> Result<i32> {
        if args.len() > 1 {
            return Err("cd: too many arguments".into());
        }
        let old = args.first().is_some_and(|s| s == "-");
        let target = if old {
            self.state.get("OLDPWD").ok_or("cd: OLDPWD is unset")?
        } else {
            args.first()
                .map(String::as_str)
                .unwrap_or_else(|| self.state.get("HOME").unwrap_or("/"))
        };
        let target = self.state.path(target);
        self.backend.directory(&target)?;
        let previous = self.state.cwd.clone();
        self.state.cwd = target.clone();
        self.state.set("OLDPWD", previous)?;
        self.state.set("PWD", target.clone())?;
        if old {
            io.write(1, &format!("{target}\n"))?;
        }
        Ok(0)
    }
    fn declare(&mut self, command: &str, args: &[String], io: &Streams) -> Result<i32> {
        if command == "local" && self.locals.is_empty() {
            return Err("local: outside a function".into());
        }
        if args.is_empty() {
            for (name, value) in &self.state.variables {
                if command == "export" && value.exported || command == "readonly" && value.readonly
                {
                    io.write(1, &format!("{command} {name}={}\n", quote(&value.value)))?;
                }
            }
            return Ok(0);
        }
        for arg in args {
            let (name, value) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(n, v)| (n, Some(v)));
            if !crate::syntax::identifier(name) {
                return Err(format!("{name}: invalid variable name"));
            }
            if command == "local"
                && let Some(scope) = self.locals.last_mut()
            {
                scope
                    .entry(name.into())
                    .or_insert_with(|| self.state.variables.get(name).cloned());
            }
            if let Some(value) = value {
                self.state.set(name, value.into())?;
            } else if !self.state.variables.contains_key(name) {
                self.state.set(name, String::new())?;
            }
            let variable = self
                .state
                .variables
                .get_mut(name)
                .ok_or("missing variable")?;
            if command == "export" {
                variable.exported = true;
            }
            if command == "readonly" {
                variable.readonly = true;
            }
        }
        Ok(0)
    }
    fn set_options(&mut self, args: &[String], io: &Streams) -> Result<i32> {
        if args.is_empty() {
            for (name, value) in &self.state.variables {
                io.write(1, &format!("{name}={}\n", quote(&value.value)))?;
            }
            return Ok(0);
        }
        let mut at = 0;
        while at < args.len() {
            let arg = &args[at];
            if arg == "--" {
                self.state.positional = args[at + 1..].to_vec();
                return Ok(0);
            }
            if !arg.starts_with(['-', '+']) {
                self.state.positional = args[at..].to_vec();
                return Ok(0);
            }
            let enabled = arg.starts_with('-');
            if arg == "-o" || arg == "+o" {
                at += 1;
                match args.get(at).map(String::as_str) {
                    Some("pipefail") => self.state.pipefail = enabled,
                    Some("nounset") => self.state.nounset = enabled,
                    Some("xtrace") => self.state.trace = enabled,
                    _ => return Err("set: options are pipefail, nounset, xtrace".into()),
                }
            } else {
                for flag in arg[1..].chars() {
                    match flag {
                        'u' => self.state.nounset = enabled,
                        'x' => self.state.trace = enabled,
                        _ => return Err(format!("set: unsupported option {flag}")),
                    }
                }
            }
            at += 1;
        }
        Ok(0)
    }
}

fn syntax_eval<B: Backend>(shell: &mut Shell<B>, source: &str, io: &Streams) -> Result<i32> {
    shell.script(
        &crate::syntax::parse(source).map_err(|e| e.to_string())?,
        io,
    )
}

pub(super) fn read_script(path: &str) -> Result<String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut source = String::new();
    file.take(crate::syntax::MAX_SCRIPT_BYTES as u64 + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() > crate::syntax::MAX_SCRIPT_BYTES {
        return Err("script exceeds size limit".into());
    }
    Ok(source)
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn test(args: &[String], state: &State) -> Result<bool> {
    match args {
        [] => Ok(false),
        [value] => Ok(!value.is_empty()),
        [not, rest @ ..] if not == "!" => Ok(!test(rest, state)?),
        [op, value] => Ok(match op.as_str() {
            "-n" => !value.is_empty(),
            "-z" => value.is_empty(),
            "-e" => std::fs::metadata(state.path(value)).is_ok(),
            "-f" => std::fs::metadata(state.path(value)).is_ok_and(|m| m.is_file()),
            "-d" => std::fs::metadata(state.path(value)).is_ok_and(|m| m.is_dir()),
            "-L" | "-h" => std::fs::symlink_metadata(state.path(value))
                .is_ok_and(|m| m.file_type().is_symlink()),
            "-s" => std::fs::metadata(state.path(value)).is_ok_and(|m| m.len() != 0),
            _ => return Err(format!("test: unsupported unary operator {op}")),
        }),
        [left, op, right] => {
            match op.as_str() {
                "=" | "==" => return Ok(left == right),
                "!=" => return Ok(left != right),
                "<" => return Ok(left < right),
                ">" => return Ok(left > right),
                _ => {}
            }
            let l: i64 = left.parse().map_err(|_| "test: invalid integer")?;
            let r: i64 = right.parse().map_err(|_| "test: invalid integer")?;
            Ok(match op.as_str() {
                "-eq" => l == r,
                "-ne" => l != r,
                "-lt" => l < r,
                "-le" => l <= r,
                "-gt" => l > r,
                "-ge" => l >= r,
                _ => return Err(format!("test: unsupported binary operator {op}")),
            })
        }
        _ => Err("test: unsupported expression; combine commands with && and ||".into()),
    }
}
