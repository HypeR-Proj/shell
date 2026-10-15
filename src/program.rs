// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Explicit command locations, separate from the shell's live working directory.

use crate::path::{CanonicalPath, Error};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authority {
    Services,
    Journal,
    Filesystem,
    Tasks,
    Handles,
    Memory,
    Activity,
    VirtualMachines,
}

pub struct Program {
    pub path: String,
    pub authority: Option<Authority>,
}

impl Program {
    pub fn is_shell(&self) -> bool {
        self.path == "/bin/sh"
    }

    pub fn resolve(name: &str, cwd: &CanonicalPath) -> Result<Self, Error> {
        let path = if name.contains('/') {
            cwd.resolve(name)?.as_str()?.to_owned()
        } else {
            CanonicalPath::root()
                .resolve(&format!("/bin/{name}"))?
                .as_str()?
                .to_owned()
        };
        // Authority follows the selected trusted program, never its basename.
        let authority = match path.as_str() {
            "/bin/componentctl" => Some(Authority::Services),
            "/bin/journalctl" => Some(Authority::Journal),
            "/bin/mount" | "/bin/umount" => Some(Authority::Filesystem),
            "/bin/ps" => Some(Authority::Tasks),
            "/bin/handle" => Some(Authority::Handles),
            "/bin/free" | "/bin/memstat" => Some(Authority::Memory),
            "/bin/top" => Some(Authority::Activity),
            "/bin/vmm" => Some(Authority::VirtualMachines),
            _ => None,
        };
        Ok(Self { path, authority })
    }
}

#[cfg(test)]
#[path = "../tests/program.rs"]
mod tests;
