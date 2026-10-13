// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Bounded wildcard matching shared by pathname expansion, case, and completion.

pub fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut i, mut j, mut star) = (0, 0, None);
    while j < t.len() {
        if p.get(i) == Some(&'*') {
            star = Some((i + 1, j));
            i += 1;
            continue;
        }
        let consumed = match p.get(i) {
            Some('?') => Some(1),
            Some('\\') => (p.get(i + 1) == Some(&t[j])).then_some(2),
            Some('[') => bracket(&p[i..], t[j]),
            Some(c) if *c == t[j] => Some(1),
            _ => None,
        };
        if let Some(n) = consumed {
            i += n;
            j += 1;
        } else if let Some((next, from)) = star {
            j = from + 1;
            i = next;
            star = Some((next, j));
        } else {
            return false;
        }
    }
    while p.get(i) == Some(&'*') {
        i += 1;
    }
    i == p.len()
}

fn bracket(pattern: &[char], c: char) -> Option<usize> {
    let mut i = 1;
    let negate = matches!(pattern.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let mut found = false;
    let start = i;
    while i < pattern.len() && (pattern[i] != ']' || i == start) {
        if pattern.get(i + 1) == Some(&'-') && pattern.get(i + 2).is_some_and(|c| *c != ']') {
            found |= pattern[i] <= c && c <= pattern[i + 2];
            i += 3;
        } else {
            found |= pattern[i] == c;
            i += 1;
        }
    }
    (i < pattern.len() && found != negate).then_some(i + 1)
}

pub(super) fn glob(pattern: &str, cwd: &str) -> super::Result<Vec<String>> {
    let absolute = pattern.starts_with('/');
    let mut paths = vec![String::new()];
    for component in pattern.split('/').filter(|s| !s.is_empty()) {
        let mut next = Vec::new();
        for prefix in paths {
            let base = if absolute {
                format!("/{prefix}")
            } else {
                super::absolute(cwd, &prefix)
            };
            if component.contains(['*', '?', '[']) {
                if let Ok(entries) = std::fs::read_dir(&base) {
                    for entry in entries {
                        let entry = entry.map_err(|e| e.to_string())?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with('.') && !component.starts_with('.') {
                            continue;
                        }
                        if matches(component, &name) {
                            next.push(join(&prefix, &name));
                        }
                        if next.len() > 4096 {
                            return Err("pathname expansion limit exceeded".into());
                        }
                    }
                }
            } else {
                let literal = unescape(component);
                if std::fs::symlink_metadata(format!("{base}/{literal}")).is_ok() {
                    next.push(join(&prefix, &literal));
                }
            }
        }
        paths = next;
    }
    if pattern.ends_with('/') {
        paths.retain(|path| {
            let path = if absolute {
                format!("/{path}")
            } else {
                super::absolute(cwd, path)
            };
            std::fs::metadata(path).is_ok_and(|m| m.is_dir())
        });
        for path in &mut paths {
            path.push('/');
        }
    }
    paths.sort();
    Ok(paths
        .into_iter()
        .map(|p| if absolute { format!("/{p}") } else { p })
        .collect())
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.into()
    } else {
        format!("{prefix}/{name}")
    }
}
pub(super) fn unescape(text: &str) -> String {
    let mut escaped = false;
    text.chars()
        .filter(|c| {
            if escaped {
                escaped = false;
                true
            } else if *c == '\\' {
                escaped = true;
                false
            } else {
                true
            }
        })
        .collect()
}
