// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::io::Streams;
use super::{Backend, Result, Shell, arithmetic, pattern};

#[derive(Default)]
struct Field {
    text: String,
    pattern: String,
    keep: bool,
}
impl Field {
    fn append(&mut self, text: &str, quoted: bool) {
        self.text.push_str(text);
        for c in text.chars() {
            if c == '\\' || quoted && matches!(c, '*' | '?' | '[') {
                self.pattern.push('\\');
            }
            self.pattern.push(c);
        }
        self.keep |= quoted || !text.is_empty();
    }
}

impl<B: Backend> Shell<B> {
    pub(super) fn expand(&mut self, word: &str, io: &Streams, split: bool) -> Result<Vec<String>> {
        self.expansion_depth += 1;
        if self.expansion_depth > 64 {
            self.expansion_depth -= 1;
            return Err("expansion nesting limit".into());
        }
        let result = self.expand_inner(word, io, split, false);
        self.expansion_depth -= 1;
        result
    }
    pub(super) fn expand_pattern(&mut self, word: &str, io: &Streams) -> Result<String> {
        Ok(self.expand_inner(word, io, false, true)?.join(" "))
    }
    fn expand_inner(
        &mut self,
        word: &str,
        io: &Streams,
        split: bool,
        pattern_only: bool,
    ) -> Result<Vec<String>> {
        let mut fields = vec![Field::default()];
        let mut quote = 0;
        let mut empty_positional_removable = false;
        let mut at = 0;
        let bytes = word.as_bytes();
        if word == "~" || word.starts_with("~/") {
            fields[0].append(self.state.get("HOME").unwrap_or("/"), true);
            at = 1;
        }
        while at < bytes.len() {
            let b = bytes[at];
            if b == b'\'' && quote != b'"' || b == b'"' && quote != b'\'' {
                if quote == 0 {
                    quote = b;
                    empty_positional_removable = !fields.last().ok_or("empty expansion")?.keep;
                    fields.last_mut().ok_or("empty expansion")?.keep = true;
                } else {
                    quote = 0;
                }
                at += 1;
                continue;
            }
            if b == b'\\' && quote != b'\'' {
                at += 1;
                let next = *bytes.get(at).ok_or("trailing escape")?;
                if next == b'\n' {
                    at += 1;
                    continue;
                }
                if quote == b'"' && !matches!(next, b'$' | b'`' | b'"' | b'\\') {
                    fields
                        .last_mut()
                        .ok_or("empty expansion")?
                        .append("\\", true);
                }
                let c = word[at..].chars().next().ok_or("invalid character")?;
                fields
                    .last_mut()
                    .ok_or("empty expansion")?
                    .append(&c.to_string(), true);
                at += c.len_utf8();
                continue;
            }
            if b == b'$' && quote != b'\'' {
                let (values, next, positional) = self.dollar(word, at, io)?;
                at = next;
                let quoted = quote != 0 || !split;
                if positional && quoted {
                    // "$@" contributes separate arguments, while prefixes and
                    // suffixes attach to its first and last positional values.
                    if values.is_empty() && empty_positional_removable {
                        let field = fields.last_mut().ok_or("empty expansion")?;
                        if field.text.is_empty() {
                            field.keep = false;
                        }
                    } else if !values.is_empty() {
                        empty_positional_removable = false;
                    }
                    for (i, value) in values.iter().enumerate() {
                        if i != 0 {
                            fields.push(Field::default());
                        }
                        fields
                            .last_mut()
                            .ok_or("empty expansion")?
                            .append(value, true);
                    }
                } else {
                    empty_positional_removable = false;
                    let value = values.join(" ");
                    if quoted {
                        fields
                            .last_mut()
                            .ok_or("empty expansion")?
                            .append(&value, true);
                    } else {
                        let ifs = self.state.get("IFS").unwrap_or(" \t\n");
                        split_append(&mut fields, &value, ifs)?;
                    }
                }
                continue;
            }
            let c = word[at..].chars().next().ok_or("invalid character")?;
            fields
                .last_mut()
                .ok_or("empty expansion")?
                .append(&c.to_string(), quote != 0);
            at += c.len_utf8();
        }
        if quote != 0 {
            return Err("unterminated quote".into());
        }
        let mut values = Vec::new();
        for field in fields {
            if pattern_only {
                values.push(field.pattern);
                continue;
            }
            if !field.keep && field.text.is_empty() && split {
                continue;
            }
            let paths = if split && field.pattern.contains(['*', '?', '[']) {
                pattern::glob(&field.pattern, &self.state.cwd)?
            } else {
                vec![]
            };
            if paths.is_empty() {
                values.push(field.text);
            } else {
                values.extend(paths);
            }
            if values.len() > 4096 {
                return Err("argument expansion limit exceeded".into());
            }
        }
        Ok(values)
    }
    fn dollar(
        &mut self,
        word: &str,
        at: usize,
        io: &Streams,
    ) -> Result<(Vec<String>, usize, bool)> {
        let bytes = word.as_bytes();
        let start = at + 1;
        if word[start..].starts_with("((") {
            let end = balanced(word, start, b'(', b')')?;
            let value = arithmetic::evaluate(&word[start + 2..end - 1], &self.state)?;
            return Ok((vec![value.to_string()], end + 1, false));
        }
        if bytes.get(start) == Some(&b'(') {
            let end = balanced(word, start, b'(', b')')?;
            let value = self.capture(&word[start + 1..end], io)?;
            return Ok((vec![value], end + 1, false));
        }
        if bytes.get(start) == Some(&b'{') {
            let end = balanced(word, start, b'{', b'}')?;
            let text = &word[start + 1..end];
            if text == "@" {
                return Ok((self.state.positional.clone(), end + 1, true));
            }
            let value = self.parameter(text, io)?;
            return Ok((vec![value], end + 1, false));
        }
        if bytes.get(start) == Some(&b'@') {
            return Ok((self.state.positional.clone(), start + 1, true));
        }
        let mut end = start;
        if bytes
            .get(start)
            .is_some_and(|b| b.is_ascii_digit() || b"?#*$-".contains(b))
        {
            end += 1;
        } else {
            while bytes
                .get(end)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                end += 1;
            }
        }
        if end == start {
            return Ok((vec!["$".into()], start, false));
        }
        Ok((vec![self.value(&word[start..end])?], end, false))
    }
    fn value(&self, name: &str) -> Result<String> {
        Ok(match name {
            "?" => self.state.status.to_string(),
            "$" => self.state.pid.to_string(),
            "#" => self.state.positional.len().to_string(),
            "0" => self.state.name.clone(),
            "*" | "@" => self.state.positional.join(
                &self
                    .state
                    .get("IFS")
                    .unwrap_or(" ")
                    .chars()
                    .next()
                    .map_or(String::new(), |c| c.to_string()),
            ),
            "-" => format!(
                "{}{}",
                if self.state.nounset { "u" } else { "" },
                if self.state.trace { "x" } else { "" }
            ),
            "PIPESTATUS" => self
                .state
                .pipeline_status
                .first()
                .copied()
                .unwrap_or(0)
                .to_string(),
            "PIPESTATUS[@]" | "PIPESTATUS[*]" => self
                .state
                .pipeline_status
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            _ => {
                let value = name
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|n| self.state.positional.get(n).map(String::as_str))
                    .or_else(|| self.state.get(name));
                match value {
                    Some(v) => v.into(),
                    None if self.state.nounset => return Err(format!("{name}: unset variable")),
                    None => String::new(),
                }
            }
        })
    }
    fn parameter(&mut self, text: &str, io: &Streams) -> Result<String> {
        if matches!(text, "?" | "#" | "-" | "$" | "0" | "*") {
            return self.value(text);
        }
        if let Some(name) = text.strip_prefix('#').filter(|name| !name.is_empty()) {
            return Ok(self.value(name)?.chars().count().to_string());
        }
        let end = text.find([':', '-', '+', '=', '?']).unwrap_or(text.len());
        let name = &text[..end];
        if end == text.len() {
            return self.value(name);
        }
        let mut operator = &text[end..];
        let colon = operator.starts_with(':');
        if colon {
            operator = &operator[1..];
        }
        let operation = operator
            .as_bytes()
            .first()
            .copied()
            .ok_or("invalid parameter expansion")?;
        let value = name
            .parse::<usize>()
            .ok()
            .filter(|n| *n != 0)
            .and_then(|n| self.state.positional.get(n - 1).map(String::as_str))
            .or_else(|| self.state.get(name));
        let set = value.is_some_and(|v| !colon || !v.is_empty());
        if (set && matches!(operation, b'-' | b'=' | b'?')) || (!set && operation == b'+') {
            return Ok(if set {
                self.value(name)?
            } else {
                String::new()
            });
        }
        let alternate = self.expand(&operator[1..], io, false)?.join(" ");
        match operation {
            b'-' | b'+' => Ok(alternate),
            b'=' => {
                self.state.set(name, alternate.clone())?;
                Ok(alternate)
            }
            b'?' => Err(if alternate.is_empty() {
                format!("{name}: unset or empty variable")
            } else {
                alternate
            }),
            _ => Err("unsupported parameter expansion".into()),
        }
    }
    pub(super) fn here_text(&mut self, text: &str, io: &Streams) -> Result<String> {
        // Here-document quotes are literal; only $, backslash-newline and
        // escapes of $, backslash and backtick have expansion meaning.
        let mut quoted = String::from("\"");
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                quoted.push('\\');
            }
            if c == '\\'
                && chars
                    .peek()
                    .is_some_and(|c| !matches!(c, '$' | '\\' | '`' | '\n'))
            {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        Ok(self.expand(&quoted, io, false)?.join(" "))
    }
}

fn split_append(fields: &mut Vec<Field>, text: &str, ifs: &str) -> Result<()> {
    let mut whitespace_delimiter = false;
    for c in text.chars() {
        let current = fields.last_mut().ok_or("empty expansion")?;
        if !ifs.contains(c) {
            current.append(&c.to_string(), false);
            whitespace_delimiter = false;
        } else if c.is_ascii_whitespace() {
            if current.keep || !current.text.is_empty() {
                fields.push(Field::default());
                whitespace_delimiter = true;
            }
        } else {
            if !(whitespace_delimiter && current.text.is_empty() && !current.keep) {
                current.keep = true;
                fields.push(Field::default());
            }
            whitespace_delimiter = false;
        }
        if fields.len() > 4096 {
            return Err("field splitting limit exceeded".into());
        }
    }
    Ok(())
}

fn balanced(text: &str, start: usize, open: u8, close: u8) -> Result<usize> {
    let bytes = text.as_bytes();
    let mut depth = 1;
    let mut at = start + 1;
    let mut quote = 0;
    while at < bytes.len() {
        let b = bytes[at];
        if b == b'\\' && quote != b'\'' {
            at += 2;
            continue;
        }
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'\'' || b == b'"' {
            quote = b;
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Ok(at);
            }
        }
        at += 1;
    }
    Err("unterminated substitution".into())
}
