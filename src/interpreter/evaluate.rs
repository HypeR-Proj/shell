// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::syntax::{self, CommandKind};
use io::{Port, Streams};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub struct Shell<B> {
    pub state: State,
    pub streams: Streams,
    pub flow: Flow,
    pub backend: B,
    pub history: Vec<String>,
    pub(super) expansion_depth: usize,
    pub(super) io_revision: u64,
    pub(super) redirection_scopes: Vec<bool>,
    substitution_status: Option<i32>,
    pub(super) call_depth: usize,
    pub(super) loop_depth: usize,
    pub(super) locals: Vec<BTreeMap<String, Option<Variable>>>,
}

impl<B: Backend> Shell<B> {
    pub fn new(state: State, backend: B) -> Self {
        Self {
            state,
            streams: Streams::default(),
            flow: Flow::None,
            backend,
            history: vec![],
            expansion_depth: 0,
            io_revision: 0,
            redirection_scopes: vec![],
            substitution_status: None,
            call_depth: 0,
            loop_depth: 0,
            locals: vec![],
        }
    }
    pub fn execute(&mut self, source: &str) -> Result<i32> {
        let script = syntax::parse(source).map_err(|e| e.to_string())?;
        let io = self.streams.clone();
        self.script(&script, &io)
    }
    pub fn worker(&mut self, worker: Worker) -> Result<i32> {
        self.state = worker.state;
        self.history = worker.history;
        self.backend.directory(&self.state.cwd)?;
        self.call_depth = worker.call_depth.min(64);
        if worker.function_scope {
            self.locals.push(BTreeMap::new());
        }
        self.command(&worker.command, &self.streams.clone())
    }
    pub(super) fn script(&mut self, script: &Script, io: &Streams) -> Result<i32> {
        let mut io = io.clone();
        let mut revision = self.io_revision;
        for item in &script.0 {
            self.run_pipeline(&item.first, &io)?;
            if revision != self.io_revision {
                io = self.streams.clone();
                revision = self.io_revision;
            }
            for (and, pipeline) in &item.rest {
                if self.flow != Flow::None {
                    break;
                }
                if *and == (self.state.status == 0) {
                    self.run_pipeline(pipeline, &io)?;
                    if revision != self.io_revision {
                        io = self.streams.clone();
                        revision = self.io_revision;
                    }
                }
            }
            if self.flow != Flow::None {
                break;
            }
        }
        Ok(self.state.status)
    }
    fn run_pipeline(&mut self, pipeline: &syntax::Pipeline, io: &Streams) -> Result<i32> {
        let start = std::time::Instant::now();
        let result = if pipeline.commands.len() == 1 {
            self.command(&pipeline.commands[0], io)
                .map(|status| vec![status])
        } else {
            let mut stages = Vec::new();
            for (i, command) in pipeline.commands.iter().enumerate() {
                let mut streams = io.clone();
                if i != 0 {
                    streams.set(
                        0,
                        Port::Pipe {
                            edge: i - 1,
                            reading: true,
                        },
                    );
                }
                if i + 1 < pipeline.commands.len() {
                    streams.set(
                        1,
                        Port::Pipe {
                            edge: i,
                            reading: false,
                        },
                    );
                }
                stages.push(self.pipeline_stage(command, streams)?);
            }
            self.backend.pipeline(stages)
        };
        let statuses = match result {
            Ok(statuses) => statuses,
            Err(error) => {
                io.write(2, &format!("sh: {error}\n"))?;
                vec![1]
            }
        };
        let status = if self.state.pipefail {
            statuses.iter().rev().find(|s| **s != 0)
        } else {
            statuses.last()
        }
        .copied()
        .unwrap_or(0);
        self.state.pipeline_status = statuses;
        self.state.status = if pipeline.negate {
            i32::from(status == 0)
        } else {
            status
        };
        self.state.last_duration_ns = start.elapsed().as_nanos();
        Ok(self.state.status)
    }
    fn pipeline_stage(&mut self, command: &Command, streams: Streams) -> Result<Stage> {
        // Plain external stages need no helper shell. Expansions that may
        // mutate variables or run a command stay inside the isolated worker;
        // their stdin must refer to an already connected pipeline.
        if let CommandKind::Simple(words) = &command.kind
            && let Some(name) = words.first()
            && !BUILTINS.contains(&name.as_str())
            && !self.state.functions.contains_key(name)
            && words
                .iter()
                .all(|w| !w.contains('$') && assignment(w).is_none())
            && command.redirects.is_empty()
        {
            let mut arguments = Vec::new();
            for word in words {
                arguments.extend(self.expand(word, &streams, true)?);
            }
            if let Some(name) = arguments.first()
                && !BUILTINS.contains(&name.as_str())
                && !self.state.functions.contains_key(name)
                && let Ok(path) = self.state.executable(name)
            {
                arguments[0] = path;
                return Ok(Stage {
                    invocation: Invocation::External {
                        arguments,
                        environment: self.state.environment(),
                    },
                    streams,
                });
            }
        }
        Ok(Stage {
            invocation: Invocation::Shell(Box::new(Worker {
                state: self.state.clone(),
                command: command.clone(),
                descriptors: vec![],
                aliases: vec![],
                history: self.history.clone(),
                call_depth: self.call_depth,
                function_scope: !self.locals.is_empty(),
            })),
            streams,
        })
    }
    fn command(&mut self, command: &Command, io: &Streams) -> Result<i32> {
        if let CommandKind::Function { name, body } = &command.kind {
            self.state.functions.insert(name.clone(), (**body).clone());
            return Ok(0);
        }
        let inherited = io;
        let global = self.streams.clone();
        let revision = self.io_revision;
        let mut io = io.clone();
        self.redirection_scopes.push(false);
        let result = self
            .apply_redirects(&command.redirects, &mut io)
            .and_then(|()| self.command_kind(&command.kind, &io));
        let committed = self
            .redirection_scopes
            .pop()
            .ok_or("missing redirection scope")?;
        if !committed && revision != self.io_revision {
            // Nested exec may retain extra descriptors, but the surrounding
            // command's temporary redirects still unwind at its boundary.
            for fd in 0..64 {
                if !io.get(fd).aliases(&inherited.get(fd)) {
                    self.streams.set(fd, global.get(fd));
                }
            }
        }
        match result {
            Ok(status) => Ok(status),
            Err(error) => {
                // A failing redirect observes all redirects already committed.
                // A closed stderr cannot turn an ordinary command error into a
                // fatal shell error or escape the selected diagnostic stream.
                let _ = io.write(2, &format!("sh: {error}\n"));
                Ok(1)
            }
        }
    }
    fn command_kind(&mut self, kind: &CommandKind, io: &Streams) -> Result<i32> {
        match kind {
            CommandKind::Simple(words) => self.simple(words, io),
            CommandKind::Group(body) => self.script(body, io),
            CommandKind::Subshell(body) => self.isolated(body, io),
            CommandKind::If {
                branches,
                otherwise,
            } => {
                let mut io = io.clone();
                for (condition, body) in branches {
                    let status = self.scoped_script(condition, &mut io)?;
                    if self.flow != Flow::None {
                        return Ok(self.state.status);
                    }
                    if status == 0 {
                        return self.script(body, &io);
                    }
                }
                self.state.status = 0;
                self.script(otherwise, &io)
            }
            CommandKind::While {
                until,
                condition,
                body,
            } => {
                self.loop_depth += 1;
                let result = (|| {
                    let mut status = 0;
                    let mut io = io.clone();
                    loop {
                        if (self.scoped_script(condition, &mut io)? == 0) == *until
                            || self.flow != Flow::None
                        {
                            break;
                        }
                        status = self.scoped_script(body, &mut io)?;
                        if self.loop_finished() {
                            break;
                        }
                    }
                    Ok(status)
                })();
                self.loop_depth -= 1;
                result
            }
            CommandKind::For { name, words, body } => {
                let mut values = Vec::new();
                if let Some(words) = words {
                    for word in words {
                        values.extend(self.expand(word, io, true)?);
                    }
                } else {
                    values = self.state.positional.clone();
                }
                self.loop_depth += 1;
                let result = (|| {
                    let mut status = 0;
                    let mut io = io.clone();
                    for value in values {
                        self.state.set(name, value)?;
                        status = self.scoped_script(body, &mut io)?;
                        if self.loop_finished() {
                            break;
                        }
                    }
                    Ok(status)
                })();
                self.loop_depth -= 1;
                result
            }
            CommandKind::Case { word, arms } => {
                let value = self.expand(word, io, false)?.join(" ");
                for (patterns, body) in arms {
                    for pattern in patterns {
                        let pattern = self.expand_pattern(pattern, io)?;
                        if pattern::matches(&pattern, &value) {
                            return self.script(body, io);
                        }
                    }
                }
                Ok(0)
            }
            CommandKind::Function { .. } => Ok(0),
        }
    }
    fn scoped_script(&mut self, script: &Script, io: &mut Streams) -> Result<i32> {
        let revision = self.io_revision;
        let result = self.script(script, io);
        if revision != self.io_revision {
            *io = self.streams.clone();
        }
        result
    }
    fn loop_finished(&mut self) -> bool {
        match self.flow {
            Flow::None => false,
            Flow::Continue(1) => {
                self.flow = Flow::None;
                false
            }
            Flow::Break(1) => {
                self.flow = Flow::None;
                true
            }
            Flow::Break(n) => {
                self.flow = Flow::Break(n - 1);
                true
            }
            Flow::Continue(n) => {
                self.flow = Flow::Continue(n - 1);
                true
            }
            _ => true,
        }
    }
    fn simple(&mut self, words: &[String], io: &Streams) -> Result<i32> {
        self.substitution_status = None;
        let assigned = words
            .iter()
            .take_while(|word| assignment(word).is_some())
            .count();
        let mut arguments = Vec::new();
        for word in &words[assigned..] {
            let declaration = arguments.first().is_some_and(|name: &String| {
                matches!(name.as_str(), "export" | "readonly" | "local")
            }) && assignment(word).is_some();
            arguments.extend(self.expand(word, io, !declaration)?);
        }
        let mut saved = Vec::new();
        let result = (|| {
            for word in &words[..assigned] {
                let (name, raw) = assignment(word).ok_or("invalid assignment")?;
                let value = self.expand(raw, io, false)?.join(" ");
                saved.push((name.to_owned(), self.state.variables.get(name).cloned()));
                self.state.set(name, value)?;
                if !arguments.is_empty()
                    && let Some(v) = self.state.variables.get_mut(name)
                {
                    v.exported = true;
                }
            }
            if arguments.is_empty() {
                return Ok(self.substitution_status.unwrap_or(0));
            }
            if self.state.trace {
                io.write(2, &format!("+ {}\n", arguments.join(" ")))?;
            }
            if let Some(body) = self.state.functions.get(&arguments[0]).cloned() {
                self.function(&body, &arguments[1..], io)
            } else if BUILTINS.contains(&arguments[0].as_str()) {
                self.builtin(&arguments, io)
            } else {
                match self.state.executable(&arguments[0]) {
                    Ok(path) => {
                        arguments[0] = path;
                        self.backend
                            .pipeline(vec![Stage {
                                invocation: Invocation::External {
                                    arguments: arguments.clone(),
                                    environment: self.state.environment(),
                                },
                                streams: io.clone(),
                            }])
                            .map(|statuses| statuses.last().copied().unwrap_or(0))
                            .or_else(|error| {
                                let _ = io.write(2, &format!("sh: {error}\n"));
                                Ok(126)
                            })
                    }
                    Err(error) => {
                        io.write(2, &format!("sh: {error}\n"))?;
                        Ok(127)
                    }
                }
            }
        })();
        if !arguments.is_empty() {
            for (name, value) in saved.into_iter().rev() {
                if let Some(value) = value {
                    self.state.variables.insert(name, value);
                } else {
                    self.state.variables.remove(&name);
                }
            }
        }
        result
    }
    fn function(&mut self, body: &Command, arguments: &[String], io: &Streams) -> Result<i32> {
        if self.call_depth == 64 {
            return Err("function call depth exceeded".into());
        }
        self.call_depth += 1;
        let saved_args = std::mem::replace(&mut self.state.positional, arguments.to_vec());
        self.locals.push(BTreeMap::new());
        let result = self.command(body, io);
        self.state.positional = saved_args;
        for (name, value) in self.locals.pop().ok_or("missing function scope")? {
            if let Some(value) = value {
                self.state.variables.insert(name, value);
            } else {
                self.state.variables.remove(&name);
            }
        }
        self.call_depth -= 1;
        if let Flow::Return(status) = self.flow {
            self.flow = Flow::None;
            Ok(status)
        } else {
            result
        }
    }
    pub(super) fn isolated(&mut self, body: &Script, io: &Streams) -> Result<i32> {
        let saved = self.state.clone();
        let saved_revision = self.io_revision;
        let flow = self.flow;
        let loops = self.loop_depth;
        let locals = self.locals.clone();
        let streams = self.streams.clone();
        self.flow = Flow::None;
        self.loop_depth = 0;
        let result = self.script(body, io);
        let result = if let Flow::Exit(status) = self.flow {
            Ok(status)
        } else {
            result
        };
        self.state = saved;
        self.io_revision = saved_revision;
        self.flow = flow;
        self.loop_depth = loops;
        self.locals = locals;
        self.streams = streams;
        self.backend.directory(&self.state.cwd)?;
        result
    }
    pub(super) fn capture(&mut self, source: &str, io: &Streams) -> Result<String> {
        let body = syntax::parse(source).map_err(|e| e.to_string())?;
        let mut io = io.clone();
        let bytes = Rc::new(RefCell::new(Vec::new()));
        io.set(1, Port::Capture(bytes.clone()));
        let status = self.isolated(&body, &io)?;
        self.substitution_status = Some(status);
        let bytes = bytes.borrow();
        let text = std::str::from_utf8(&bytes).map_err(|_| "command substitution is not UTF-8")?;
        if text.contains('\0') {
            return Err("command substitution contains NUL".into());
        }
        Ok(text.trim_end_matches('\n').into())
    }
    fn apply_redirects(&mut self, redirects: &[Redirect], io: &mut Streams) -> Result<()> {
        for redirect in redirects {
            let op = redirect.op.as_str();
            let port = if matches!(op, "<<" | "<<-") {
                let here = redirect.here.as_ref().ok_or("missing here-document")?;
                let body = if here.expand {
                    self.here_text(&here.body, io)?
                } else {
                    here.body.clone()
                };
                Port::memory(body.into_bytes())
            } else if op == "<<<" {
                let mut body = self.expand(&redirect.word, io, false)?.join(" ");
                body.push('\n');
                Port::memory(body.into_bytes())
            } else {
                let words = self.expand(&redirect.word, io, true)?;
                if words.len() != 1 {
                    return Err("ambiguous redirection".into());
                }
                let word = &words[0];
                if matches!(op, "<&" | ">&") {
                    if word == "-" {
                        Port::Closed
                    } else {
                        let moved = word.ends_with('-');
                        let source: u8 = word
                            .trim_end_matches('-')
                            .parse()
                            .map_err(|_| "invalid descriptor duplication")?;
                        let port = io.get(source);
                        if matches!(port, Port::Closed) {
                            return Err(format!("{source}: descriptor is closed"));
                        }
                        if moved {
                            io.set(source, Port::Closed);
                        }
                        port
                    }
                } else {
                    let path = self.state.path(word);
                    let mut options = std::fs::OpenOptions::new();
                    match op {
                        "<" => {
                            options.read(true);
                        }
                        "<>" => {
                            options.read(true).write(true).create(true);
                        }
                        ">>" | "&>>" => {
                            options.write(true).append(true).create(true);
                        }
                        ">" | ">|" | "&>" => {
                            options.write(true).create(true).truncate(true);
                        }
                        _ => return Err("unknown redirection".into()),
                    }
                    Port::File(Rc::new(RefCell::new(
                        options.open(&path).map_err(|e| format!("{word}: {e}"))?,
                    )))
                }
            };
            if matches!(op, "&>" | "&>>") {
                io.set(2, port.clone());
            }
            io.set(redirect.fd, port);
        }
        Ok(())
    }
}
