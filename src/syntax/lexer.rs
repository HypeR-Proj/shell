// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::{Error, HereDocument, MAX_DEPTH};

#[derive(Clone, Debug)]
pub(super) struct Token {
    pub text: String,
    pub operator: bool,
    pub start: usize,
    pub end: usize,
    pub here: Option<HereDocument>,
}

const OPERATORS: &[&str] = &[
    "<<-", "<<<", "&>>", "&&", "||", ">>", "<<", "<&", ">&", "<>", ">|", "&>", ";;", "|&", "|",
    "&", ";", "(", ")", "<", ">", "\n",
];

pub(super) fn lex(source: &str) -> Result<Vec<Token>, Error> {
    let bytes = source.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut pending: Vec<(usize, String, bool, bool)> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if matches!(bytes[at], b' ' | b'\t' | b'\r') {
            at += 1;
            continue;
        }
        if source[at..].starts_with("\\\n") {
            at += 2;
            continue;
        }
        if bytes[at] == b'#' {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        let start = at;
        let operator = OPERATORS.iter().find(|op| source[at..].starts_with(**op));
        if let Some(op) = operator {
            at += op.len();
            tokens.push(Token {
                text: (*op).to_owned(),
                operator: true,
                start,
                end: at,
                here: None,
            });
            if *op == "\n" {
                for (index, delimiter, strip_tabs, expand) in pending.drain(..) {
                    let mut body = String::new();
                    loop {
                        if at == bytes.len() {
                            return Err(Error::new(
                                at,
                                format!("expected here-document delimiter {delimiter:?}"),
                                true,
                            ));
                        }
                        let end = source[at..].find('\n').map_or(bytes.len(), |n| at + n);
                        let line = &source[at..end];
                        let line = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            line
                        };
                        at = (end + 1).min(bytes.len());
                        if line == delimiter {
                            break;
                        }
                        body.push_str(line);
                        if end < bytes.len() {
                            body.push('\n');
                        }
                    }
                    tokens[index].here = Some(HereDocument { body, expand });
                }
            }
        } else {
            at = word_end(source, at)?;
            let text = source[start..at].to_owned();
            if let Some(last) = tokens.last()
                && last.operator
                && matches!(last.text.as_str(), "<<" | "<<-")
            {
                let delimiter = shlex::split(&text)
                    .and_then(|mut words| (words.len() == 1).then(|| words.remove(0)))
                    .ok_or_else(|| Error::new(start, "invalid here-document delimiter", false))?;
                pending.push((
                    tokens.len() - 1,
                    delimiter,
                    last.text == "<<-",
                    !text.contains(['\'', '"', '\\']),
                ));
            }
            tokens.push(Token {
                text,
                operator: false,
                start,
                end: at,
                here: None,
            });
        }
        if tokens.len() > 65536 {
            return Err(Error::new(at, "too many shell tokens", false));
        }
    }
    if !pending.is_empty() {
        return Err(Error::new(at, "expected here-document body", true));
    }
    Ok(tokens)
}

// A word owns balanced substitutions. Operators inside them belong to their
// nested parser, never the surrounding command or pipeline.
fn word_end(source: &str, mut at: usize) -> Result<usize, Error> {
    let bytes = source.as_bytes();
    let mut quote = 0;
    let mut stack: Vec<(u8, u8)> = Vec::new();
    while at < bytes.len() {
        let b = bytes[at];
        if b == b'\\' && quote != b'\'' {
            at += 2;
            if at > bytes.len() {
                return Err(Error::new(at - 1, "trailing backslash", true));
            }
            continue;
        }
        if quote == b'\'' {
            if b == quote {
                quote = 0;
            }
            at += 1;
            continue;
        }
        if b == b'$' && matches!(bytes.get(at + 1), Some(b'(' | b'{')) {
            if stack.len() == MAX_DEPTH {
                return Err(Error::new(at, "substitution nesting limit", false));
            }
            stack.push((if bytes[at + 1] == b'(' { b')' } else { b'}' }, quote));
            quote = 0;
            at += 2;
            continue;
        }
        if quote == 0
            && let Some((end, saved)) = stack.last().copied()
        {
            if b == end {
                stack.pop();
                quote = saved;
                at += 1;
                continue;
            }
            if b == b'(' && end == b')' {
                stack.push((b')', 0));
                at += 1;
                continue;
            }
        }
        if b == b'"' {
            quote = if quote == b'"' { 0 } else { b'"' };
            at += 1;
            continue;
        }
        if b == b'\'' && quote == 0 {
            quote = b;
            at += 1;
            continue;
        }
        if quote == 0 && stack.is_empty() && (b" \t\r\n|&;()<>".contains(&b)) {
            break;
        }
        if b == b'`' {
            return Err(Error::new(at, "use $(...) for command substitution", false));
        }
        at += 1;
    }
    if quote != 0 || !stack.is_empty() {
        return Err(Error::new(at, "unterminated quote or substitution", true));
    }
    Ok(at)
}
