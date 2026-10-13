// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! The read builtin preserves escaped separators and shared descriptor offsets.

use super::*;
use io::Streams;

impl<B: Backend> Shell<B> {
    pub(super) fn read(&mut self, args: &[String], io: &Streams) -> Result<i32> {
        let (mut raw, mut fd, mut at) = (false, 0, 0);
        while let Some(arg) = args.get(at) {
            match arg.as_str() {
                "-r" => raw = true,
                "-u" => {
                    at += 1;
                    fd = args
                        .get(at)
                        .ok_or("read: expected descriptor")?
                        .parse()
                        .map_err(|_| "read: invalid descriptor")?;
                }
                "-p" => {
                    at += 1;
                    io.write(2, args.get(at).ok_or("read: expected prompt")?)?;
                }
                "--" => {
                    at += 1;
                    break;
                }
                _ if arg.starts_with('-') => return Err(format!("read: unsupported option {arg}")),
                _ => break,
            }
            at += 1;
        }
        let names = &args[at..];
        let terminal = matches!(io.get(fd), super::io::Port::Standard(0));
        if terminal {
            self.backend.canonical_input(true)?;
        }
        let result = (|| -> Result<(Vec<u8>, Vec<bool>, bool)> {
            let mut line = Vec::new();
            let mut protected = Vec::new();
            let mut eof = false;
            let mut escaped = false;
            loop {
                let mut b = [0];
                if io.get(fd).read(&mut b)? == 0 {
                    eof = true;
                    break;
                }
                if !raw && !escaped && b[0] == b'\\' {
                    escaped = true;
                    continue;
                }
                if b[0] == b'\n' {
                    if escaped {
                        escaped = false;
                        continue;
                    }
                    break;
                }
                line.push(b[0]);
                protected.push(escaped);
                escaped = false;
                if line.len() > crate::syntax::MAX_SCRIPT_BYTES {
                    return Err("read: line too large".into());
                }
            }
            Ok((line, protected, eof))
        })();
        if terminal {
            self.backend.canonical_input(false)?;
        }
        let (line, protected, eof) = result?;
        let line = String::from_utf8(line).map_err(|_| "read: input is not UTF-8")?;
        if names.is_empty() {
            self.state.set("REPLY", line)?;
        } else {
            let ifs = self.state.get("IFS").unwrap_or(" \t\n").to_owned();
            let values = fields(&line, &protected, &ifs, names.len());
            for (name, value) in names.iter().zip(values) {
                self.state.set(name, value.into())?;
            }
        }
        Ok(i32::from(eof))
    }
}

fn fields<'a>(line: &'a str, protected: &[bool], ifs: &str, count: usize) -> Vec<&'a str> {
    let chars: Vec<_> = line.char_indices().collect();
    let separator = |i: usize| !protected[chars[i].0] && ifs.contains(chars[i].1);
    let whitespace = |i: usize| separator(i) && matches!(chars[i].1, ' ' | '\t' | '\n');
    let offset = |i: usize| chars.get(i).map_or(line.len(), |(at, _)| *at);
    let mut at = 0;
    while at < chars.len() && whitespace(at) {
        at += 1;
    }
    let mut ranges = Vec::new();
    while at < chars.len() {
        let start = offset(at);
        while at < chars.len() && !separator(at) {
            at += 1;
        }
        ranges.push(start..offset(at));
        while at < chars.len() && whitespace(at) {
            at += 1;
        }
        if at < chars.len() && separator(at) {
            at += 1;
        }
        while at < chars.len() && whitespace(at) {
            at += 1;
        }
    }
    let mut end = chars.len();
    while end > 0 && whitespace(end - 1) {
        end -= 1;
    }
    (0..count)
        .map(|i| match ranges.get(i) {
            // The final name receives all remaining fields with their original
            // separators. Only unescaped IFS whitespace is stripped at the end.
            Some(range) if i + 1 == count && i + 1 < ranges.len() => {
                &line[range.start..offset(end)]
            }
            Some(range) => &line[range.clone()],
            None => "",
        })
        .collect()
}
