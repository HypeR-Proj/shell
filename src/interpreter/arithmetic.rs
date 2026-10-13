// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::{Result, State};

pub(super) fn evaluate(source: &str, state: &State) -> Result<i64> {
    let mut parser = Parser {
        source,
        at: 0,
        state,
        depth: 0,
    };
    let value = parser.expression(0, true)?;
    parser.spaces();
    if parser.at != source.len() {
        return Err("unexpected arithmetic token".into());
    }
    Ok(value)
}

struct Parser<'a> {
    source: &'a str,
    at: usize,
    state: &'a State,
    depth: usize,
}
impl Parser<'_> {
    fn spaces(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.at)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.at += 1;
        }
    }
    fn take(&mut self, text: &str) -> bool {
        self.spaces();
        if self.source[self.at..].starts_with(text) {
            self.at += text.len();
            true
        } else {
            false
        }
    }
    fn expression(&mut self, minimum: u8, active: bool) -> Result<i64> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("arithmetic nesting limit".into());
        }
        let mut left = if self.take("(") {
            let value = self.expression(0, active)?;
            if !self.take(")") {
                return Err("expected arithmetic ')'".into());
            }
            value
        } else if self.take("+") {
            self.expression(11, active)?
        } else if self.take("-") {
            self.expression(11, active)?.wrapping_neg()
        } else if self.take("!") {
            i64::from(self.expression(11, active)? == 0)
        } else if self.take("~") {
            !self.expression(11, active)?
        } else {
            self.value(active)?
        };
        loop {
            self.spaces();
            let operator = [
                ("||", 1),
                ("&&", 2),
                ("==", 6),
                ("!=", 6),
                ("<=", 7),
                (">=", 7),
                ("<<", 8),
                (">>", 8),
                ("|", 3),
                ("^", 4),
                ("&", 5),
                ("<", 7),
                (">", 7),
                ("+", 9),
                ("-", 9),
                ("*", 10),
                ("/", 10),
                ("%", 10),
            ]
            .into_iter()
            .find(|(op, _)| self.source[self.at..].starts_with(op));
            let Some((op, precedence)) = operator else {
                break;
            };
            if precedence < minimum {
                break;
            }
            self.at += op.len();
            let enabled = active && !(op == "&&" && left == 0 || op == "||" && left != 0);
            let right = self.expression(precedence + 1, enabled)?;
            if active {
                left = match op {
                    "||" => i64::from(left != 0 || right != 0),
                    "&&" => i64::from(left != 0 && right != 0),
                    "|" => left | right,
                    "^" => left ^ right,
                    "&" => left & right,
                    "==" => i64::from(left == right),
                    "!=" => i64::from(left != right),
                    "<" => i64::from(left < right),
                    ">" => i64::from(left > right),
                    "<=" => i64::from(left <= right),
                    ">=" => i64::from(left >= right),
                    "+" => left.wrapping_add(right),
                    "-" => left.wrapping_sub(right),
                    "*" => left.wrapping_mul(right),
                    "/" | "%" if right == 0 => return Err("arithmetic division by zero".into()),
                    "/" => left.wrapping_div(right),
                    "%" => left.wrapping_rem(right),
                    "<<" => left.wrapping_shl(right as u32),
                    ">>" => left.wrapping_shr(right as u32),
                    _ => return Err("invalid arithmetic operator".into()),
                };
            }
        }
        self.depth -= 1;
        Ok(left)
    }
    fn value(&mut self, active: bool) -> Result<i64> {
        self.spaces();
        let start = self.at;
        while self
            .source
            .as_bytes()
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            self.at += 1;
        }
        let token = &self.source[start..self.at];
        if token.is_empty() {
            return Err("expected arithmetic value".into());
        }
        if !active {
            return Ok(0);
        }
        let value = if crate::syntax::identifier(token) {
            self.state.get(token).unwrap_or("0")
        } else {
            token
        };
        let value = if value.is_empty() { "0" } else { value };
        if let Some(hex) = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            i64::from_str_radix(hex, 16)
        } else {
            value.parse()
        }
        .map_err(|_| format!("{value}: invalid arithmetic integer"))
    }
}
