// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Bounded builtin formatting. Escape decoding writes bytes, including NUL and
//! non-UTF-8 data, without a per-character terminal request.

use super::{Result, Streams};

pub(super) fn echo(args: &[String], io: &Streams) -> Result<i32> {
    let (mut newline, mut decode, mut at) = (true, false, 0);
    while let Some(arg) = args.get(at) {
        if arg == "--" {
            at += 1;
            break;
        }
        if arg.len() < 2
            || !arg.starts_with('-')
            || !arg[1..].chars().all(|c| matches!(c, 'n' | 'e' | 'E'))
        {
            break;
        }
        for c in arg[1..].chars() {
            match c {
                'n' => newline = false,
                'e' => decode = true,
                'E' => decode = false,
                _ => {}
            }
        }
        at += 1;
    }
    let text = args[at..].join(" ");
    let (mut bytes, stop) = if decode {
        escapes(text.as_bytes(), false)
    } else {
        (text.into_bytes(), false)
    };
    if newline && !stop {
        bytes.push(b'\n');
    }
    io.get(1).write(&bytes)?;
    Ok(0)
}

pub(super) fn printf(args: &[String], io: &Streams) -> Result<()> {
    let args = if args.first().is_some_and(|s| s == "--") {
        &args[1..]
    } else {
        args
    };
    let format = args.first().ok_or("printf: expected a format")?;
    let (format, stop) = escapes(format.as_bytes(), true);
    let mut argument = 1;
    let mut output = Vec::new();
    'repeat: loop {
        let start = argument;
        let mut at = 0;
        while at < format.len() {
            let byte = format[at];
            at += 1;
            if byte != b'%' {
                output.push(byte);
                continue;
            }
            if format.get(at) == Some(&b'%') {
                output.push(b'%');
                at += 1;
                continue;
            }
            let flags_start = at;
            while format.get(at).is_some_and(|c| b"-+ 0#".contains(c)) {
                at += 1;
            }
            let flags = &format[flags_start..at];
            let width = number(&format, &mut at)?;
            let precision = if format.get(at) == Some(&b'.') {
                at += 1;
                Some(number(&format, &mut at)?)
            } else {
                None
            };
            let spec = *format.get(at).ok_or("printf: incomplete format")?;
            at += 1;
            let value = args.get(argument).map(String::as_str).unwrap_or("");
            argument += 1;
            let (mut bytes, stop_value) = match spec {
                b's' => (
                    value.as_bytes()[..precision.unwrap_or(value.len()).min(value.len())].to_vec(),
                    false,
                ),
                b'c' => (
                    value
                        .chars()
                        .next()
                        .map_or(Vec::new(), |c| c.to_string().into_bytes()),
                    false,
                ),
                b'b' => escapes(value.as_bytes(), false),
                b'q' => (
                    shlex::try_quote(value)
                        .map_err(|e| e.to_string())?
                        .as_bytes()
                        .to_vec(),
                    false,
                ),
                b'd' | b'i' | b'u' | b'x' | b'X' | b'o' => {
                    let n = integer(value)?;
                    let text = match spec {
                        b'x' => format!("{:x}", n as u64),
                        b'X' => format!("{:X}", n as u64),
                        b'o' => format!("{:o}", n as u64),
                        b'u' => (n as u64).to_string(),
                        _ => n.to_string(),
                    };
                    let sign = if text.starts_with('-') {
                        "-"
                    } else if flags.contains(&b'+') {
                        "+"
                    } else if flags.contains(&b' ') {
                        " "
                    } else {
                        ""
                    };
                    let digits = text.trim_start_matches('-');
                    let prefix = if flags.contains(&b'#') && n != 0 {
                        match spec {
                            b'x' => "0x",
                            b'X' => "0X",
                            b'o' => "0",
                            _ => "",
                        }
                    } else {
                        ""
                    };
                    let zeroes = precision.unwrap_or(0).saturating_sub(digits.len());
                    (
                        format!("{sign}{prefix}{}{digits}", "0".repeat(zeroes)).into_bytes(),
                        false,
                    )
                }
                _ => return Err(format!("printf: unsupported format %{}", char::from(spec))),
            };
            let padding = width.saturating_sub(bytes.len());
            if !flags.contains(&b'-') {
                let pad = if flags.contains(&b'0')
                    && !matches!(spec, b's' | b'c' | b'b' | b'q')
                    && precision.is_none()
                {
                    b'0'
                } else {
                    b' '
                };
                if pad == b'0'
                    && bytes
                        .first()
                        .is_some_and(|b| matches!(b, b'-' | b'+' | b' '))
                {
                    output.push(bytes.remove(0));
                }
                output.extend(std::iter::repeat_n(pad, padding));
            }
            output.extend(bytes);
            if flags.contains(&b'-') {
                output.extend(std::iter::repeat_n(b' ', padding));
            }
            if output.len() > crate::syntax::MAX_SCRIPT_BYTES {
                return Err("printf: output limit exceeded".into());
            }
            if stop_value {
                break 'repeat;
            }
        }
        if stop || argument == start || argument >= args.len() {
            break;
        }
    }
    io.get(1).write(&output)
}

fn number(bytes: &[u8], at: &mut usize) -> Result<usize> {
    let mut n = 0;
    while let Some(byte) = bytes.get(*at).filter(|b| b.is_ascii_digit()) {
        n = n * 10 + usize::from(*byte - b'0');
        *at += 1;
        if n > 4096 {
            return Err("printf: field width or precision limit exceeded".into());
        }
    }
    Ok(n)
}

fn integer(text: &str) -> Result<i64> {
    if text.is_empty() {
        return Ok(0);
    }
    if text.starts_with(['\'', '"']) {
        return Ok(text[1..].chars().next().map_or(0, |c| c as i64));
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16)
    } else if text.len() > 1 && text.starts_with('0') {
        i64::from_str_radix(&text[1..], 8)
    } else {
        text.parse()
    }
    .map_err(|_| format!("printf: invalid integer {text:?}"))
}

fn escapes(bytes: &[u8], format: bool) -> (Vec<u8>, bool) {
    let mut output = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        at += 1;
        if byte != b'\\' || at == bytes.len() {
            output.push(byte);
            continue;
        }
        let escaped = bytes[at];
        at += 1;
        let decoded = match escaped {
            b'a' => 7,
            b'b' => 8,
            b'e' | b'E' => 27,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            b'\\' => b'\\',
            b'c' => return (output, true),
            b'0'..=b'7' | b'x' if escaped == b'x' || escaped == b'0' || format => {
                let radix = if escaped == b'x' { 16 } else { 8 };
                let limit = if radix == 16 {
                    2
                } else if format {
                    at -= 1;
                    3
                } else {
                    3
                };
                let start = at;
                let mut n = 0_u32;
                while at - start < limit {
                    let Some(digit) = bytes.get(at).and_then(|b| char::from(*b).to_digit(radix))
                    else {
                        break;
                    };
                    n = n * radix + digit;
                    at += 1;
                }
                if at == start && escaped == b'x' {
                    output.extend_from_slice(b"\\x");
                    continue;
                }
                n as u8
            }
            _ => {
                output.extend_from_slice(&[b'\\', escaped]);
                continue;
            }
        };
        output.push(decoded);
    }
    (output, false)
}
