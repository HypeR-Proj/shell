// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Lexical paths inside the shell's delegated root capability.

use hyper_os::fs::{MAX_NAME_BYTES, MAX_PATH_BYTES};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Empty,
    InvalidComponent,
    TooLong,
}

#[derive(Clone)]
pub struct CanonicalPath(PathBuf);

impl CanonicalPath {
    pub fn root() -> Self {
        Self(PathBuf::from("/"))
    }

    pub fn resolve(&self, input: &str) -> Result<Self, Error> {
        if input.is_empty() {
            return Err(Error::Empty);
        }
        let mut resolved = self.clone();
        for component in Path::new(input).components() {
            match component {
                Component::RootDir => resolved = Self::root(),
                Component::CurDir => {}
                Component::ParentDir => {
                    resolved.0.pop();
                }
                Component::Normal(value) => {
                    let bytes = value.as_encoded_bytes();
                    if bytes.len() > MAX_NAME_BYTES || bytes.contains(&0) {
                        return Err(Error::InvalidComponent);
                    }
                    resolved.0.push(value);
                    if resolved.0.as_os_str().len() > MAX_PATH_BYTES {
                        return Err(Error::TooLong);
                    }
                }
                Component::Prefix(_) => return Err(Error::InvalidComponent),
            }
        }
        Ok(resolved)
    }

    pub fn as_str(&self) -> Result<&str, Error> {
        self.0.to_str().ok_or(Error::InvalidComponent)
    }
}

#[cfg(test)]
#[path = "../tests/path.rs"]
mod tests;
