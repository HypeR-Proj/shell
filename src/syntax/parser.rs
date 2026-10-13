// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::lexer::Token;
use super::*;

pub(super) fn parse(tokens: Vec<Token>) -> Result<Script, Error> {
    let mut parser = Parser {
        tokens,
        at: 0,
        depth: 0,
    };
    let script = parser.script(&[])?;
    if parser.at != parser.tokens.len() {
        return Err(parser.error("unexpected token"));
    }
    Ok(script)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &str {
        self.tokens.get(self.at).map_or("", |t| t.text.as_str())
    }
    fn eat(&mut self, text: &str) -> bool {
        if self.peek() == text && self.at < self.tokens.len() {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn error(&self, message: &str) -> Error {
        Error::new(
            self.tokens
                .get(self.at)
                .map_or_else(|| self.tokens.last().map_or(0, |t| t.end), |t| t.start),
            message,
            self.at == self.tokens.len(),
        )
    }
    fn require(&mut self, text: &str) -> Result<(), Error> {
        if self.eat(text) {
            Ok(())
        } else {
            Err(self.error(&format!("expected {text:?}")))
        }
    }
    fn newlines(&mut self) {
        while self.eat("\n") {}
    }
    fn word(&mut self) -> Result<String, Error> {
        let token = self
            .tokens
            .get(self.at)
            .filter(|t| !t.operator)
            .ok_or_else(|| self.error("expected a word"))?;
        let text = token.text.clone();
        self.at += 1;
        Ok(text)
    }
    fn script(&mut self, stops: &[&str]) -> Result<Script, Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("command nesting limit"));
        }
        let mut items = Vec::new();
        self.newlines();
        while self.at < self.tokens.len() && !stops.contains(&self.peek()) {
            let first = self.pipeline()?;
            let mut rest = Vec::new();
            while matches!(self.peek(), "&&" | "||") {
                let and = self.eat("&&");
                if !and {
                    self.require("||")?;
                }
                self.newlines();
                rest.push((and, self.pipeline()?));
            }
            items.push(AndOr { first, rest });
            if self.eat(";") || self.eat("\n") {
                self.newlines();
            } else if self.at < self.tokens.len() && !stops.contains(&self.peek()) {
                return Err(self.error(if self.peek() == "&" {
                    "background jobs are not supported"
                } else {
                    "expected a command separator"
                }));
            }
        }
        self.depth -= 1;
        Ok(Script(items))
    }
    fn pipeline(&mut self) -> Result<Pipeline, Error> {
        let negate = self.eat("!");
        let mut commands = vec![self.command()?];
        while matches!(self.peek(), "|" | "|&") {
            let stderr = self.eat("|&");
            if !stderr {
                self.require("|")?;
            }
            if stderr && let Some(command) = commands.last_mut() {
                command.redirects.push(Redirect {
                    fd: 2,
                    op: ">&".into(),
                    word: "1".into(),
                    here: None,
                });
            }
            if commands.len() == 32 {
                return Err(self.error("pipeline stage limit"));
            }
            self.newlines();
            commands.push(self.command()?);
        }
        Ok(Pipeline { negate, commands })
    }
    fn command(&mut self) -> Result<Command, Error> {
        let kind = match self.peek() {
            "if" => self.if_command()?,
            "while" | "until" => {
                let until = self.eat("until");
                if !until {
                    self.require("while")?;
                }
                let condition = self.script(&["do"])?;
                if condition.0.is_empty() {
                    return Err(self.error("expected a loop condition"));
                }
                self.require("do")?;
                let body = self.script(&["done"])?;
                self.require("done")?;
                CommandKind::While {
                    until,
                    condition,
                    body,
                }
            }
            "for" => {
                self.at += 1;
                let name = self.word()?;
                if !identifier(&name) {
                    return Err(self.error("invalid loop variable"));
                }
                self.newlines();
                let words = if self.eat("in") {
                    let mut words = Vec::new();
                    while !matches!(self.peek(), "" | ";" | "\n") {
                        words.push(self.word()?);
                    }
                    Some(words)
                } else {
                    None
                };
                if !self.eat(";") && self.peek() != "\n" && self.peek() != "do" {
                    return Err(self.error("expected loop separator"));
                }
                self.newlines();
                self.require("do")?;
                let body = self.script(&["done"])?;
                self.require("done")?;
                CommandKind::For { name, words, body }
            }
            "case" => {
                self.at += 1;
                let word = self.word()?;
                self.newlines();
                self.require("in")?;
                let mut arms = Vec::new();
                self.newlines();
                while !self.eat("esac") {
                    self.eat("(");
                    let mut patterns = vec![self.word()?];
                    while self.eat("|") {
                        patterns.push(self.word()?);
                    }
                    self.require(")")?;
                    let body = self.script(&[";;", "esac"])?;
                    arms.push((patterns, body));
                    if !self.eat(";;") && self.peek() != "esac" {
                        return Err(self.error("expected ;; or esac"));
                    }
                    self.newlines();
                }
                CommandKind::Case { word, arms }
            }
            "{" | "(" => {
                let subshell = self.eat("(");
                if !subshell {
                    self.require("{")?;
                }
                let end = if subshell { ")" } else { "}" };
                let body = self.script(&[end])?;
                self.require(end)?;
                if subshell {
                    CommandKind::Subshell(body)
                } else {
                    CommandKind::Group(body)
                }
            }
            "then" | "else" | "elif" | "fi" | "do" | "done" | "}" | "esac" => {
                return Err(self.error("unexpected reserved word"));
            }
            _ => {
                if self
                    .tokens
                    .get(self.at)
                    .is_some_and(|t| !t.operator && identifier(&t.text))
                    && self.tokens.get(self.at + 1).is_some_and(|t| t.text == "(")
                    && self.tokens.get(self.at + 2).is_some_and(|t| t.text == ")")
                {
                    let name = self.word()?;
                    self.at += 2;
                    self.newlines();
                    if !matches!(self.peek(), "{" | "(") {
                        return Err(self.error("function body must be a compound command"));
                    }
                    return Ok(Command {
                        kind: CommandKind::Function {
                            name,
                            body: Box::new(self.command()?),
                        },
                        redirects: Vec::new(),
                    });
                }
                let mut words = Vec::new();
                let mut redirects = Vec::new();
                loop {
                    if let Some(redirect) = self.redirect()? {
                        redirects.push(redirect);
                    } else if self
                        .tokens
                        .get(self.at)
                        .is_some_and(|t| !t.operator && t.text != "}")
                    {
                        words.push(self.word()?);
                    } else {
                        break;
                    }
                }
                if words.is_empty() && redirects.is_empty() {
                    return Err(self.error("expected a command"));
                }
                return Ok(Command {
                    kind: CommandKind::Simple(words),
                    redirects,
                });
            }
        };
        let mut redirects = Vec::new();
        while let Some(redirect) = self.redirect()? {
            redirects.push(redirect);
        }
        Ok(Command { kind, redirects })
    }
    fn if_command(&mut self) -> Result<CommandKind, Error> {
        self.require("if")?;
        let mut branches = Vec::new();
        loop {
            let condition = self.script(&["then"])?;
            if condition.0.is_empty() {
                return Err(self.error("expected an if condition"));
            }
            self.require("then")?;
            let body = self.script(&["elif", "else", "fi"])?;
            branches.push((condition, body));
            if !self.eat("elif") {
                break;
            }
        }
        let otherwise = if self.eat("else") {
            self.script(&["fi"])?
        } else {
            Script::default()
        };
        self.require("fi")?;
        Ok(CommandKind::If {
            branches,
            otherwise,
        })
    }
    fn redirect(&mut self) -> Result<Option<Redirect>, Error> {
        let Some(token) = self.tokens.get(self.at) else {
            return Ok(None);
        };
        let mut number = None;
        let mut index = self.at;
        if !token.operator
            && token.text.bytes().all(|b| b.is_ascii_digit())
            && self.tokens.get(index + 1).is_some_and(|next| {
                next.start == token.end
                    && next.operator
                    && matches!(next.text.as_bytes().first(), Some(b'<' | b'>'))
            })
        {
            number = Some(
                token
                    .text
                    .parse::<u8>()
                    .ok()
                    .filter(|n| *n < 64)
                    .ok_or_else(|| self.error("descriptor must be between 0 and 63"))?,
            );
            index += 1;
        }
        let token = &self.tokens[index];
        if !token.operator
            || !matches!(
                token.text.as_str(),
                "<" | ">" | ">>" | ">|" | "<>" | "<&" | ">&" | "<<" | "<<-" | "<<<" | "&>" | "&>>"
            )
        {
            return Ok(None);
        }
        let op = token.text.clone();
        let here = token.here.clone();
        let fd = number.unwrap_or(if op.starts_with('<') { 0 } else { 1 });
        self.at = index + 1;
        let word = self.word()?;
        Ok(Some(Redirect { fd, op, word, here }))
    }
}
