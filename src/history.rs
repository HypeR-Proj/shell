// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Volatile history only. No discovery or fallback to a persistent home volume.

use std::io::{Read, Write};

pub const PATH: &str = "/tmp/hyper-sh.history";
pub const MAX_ENTRIES: usize = 256;
const MAX_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub struct History {
    entries: Vec<String>,
}

impl History {
    pub fn load() -> Self {
        Self::load_path(std::path::Path::new(PATH))
    }
    fn load_path(path: &std::path::Path) -> Self {
        // Reject symlinks: history must not turn /tmp into a route to a
        // persistent file selected by another process.
        if !std::fs::symlink_metadata(path)
            .is_ok_and(|m| m.is_file() && m.len() <= MAX_BYTES as u64)
        {
            return Self::default();
        }
        let mut text = String::new();
        if std::fs::File::open(path)
            .and_then(|f| f.take(MAX_BYTES as u64 + 1).read_to_string(&mut text))
            .is_err()
            || text.len() > MAX_BYTES
        {
            return Self::default();
        }
        let entries = serde_json::from_str::<Vec<String>>(&text).unwrap_or_default();
        let mut history = Self::default();
        for entry in entries {
            history.push(entry);
        }
        history
    }
    pub fn entries(&self) -> &[String] {
        &self.entries
    }
    pub fn push(&mut self, entry: String) {
        if entry.len() > crate::syntax::MAX_SCRIPT_BYTES
            || entry.as_bytes().first().is_none_or(u8::is_ascii_whitespace)
            || self.entries.last() == Some(&entry)
        {
            return;
        }
        self.entries.push(entry);
        while self.entries.len() > MAX_ENTRIES
            || self.entries.iter().map(String::len).sum::<usize>() > MAX_BYTES / 2
        {
            self.entries.remove(0);
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn save(&self) -> std::io::Result<()> {
        self.save_path(std::path::Path::new(PATH))
    }
    fn save_path(&self, path: &std::path::Path) -> std::io::Result<()> {
        // Create a distinct staging file without following an existing link;
        // rename replaces the directory entry, never its possible link target.
        let mut serial = 0_u32;
        let (temporary, mut file) = loop {
            let temporary = format!(
                "{}.{}.{serial}.tmp",
                path.display(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => break (temporary, file),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && serial < 16 => {
                    serial += 1;
                }
                Err(e) => return Err(e),
            }
        };
        let result = (|| {
            let bytes = serde_json::to_vec(&self.entries).map_err(std::io::Error::other)?;
            if bytes.len() > MAX_BYTES {
                return Err(std::io::Error::other("history size limit"));
            }
            file.write_all(&bytes)?;
            drop(file);
            std::fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

#[cfg(test)]
#[path = "../tests/history.rs"]
mod tests;
