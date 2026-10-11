// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Explicit command locations, separate from the shell's live working directory.

use crate::path::{CanonicalPath, Error};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageSource {
    Environment,
    Namespace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEnvironment {
    Recovery,
    System,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authority {
    Filesystem,
    Tasks,
    Handles,
    Memory,
    Activity,
    VirtualMachines,
}

pub struct Program {
    pub path: String,
    pub source: ImageSource,
    pub environment: RuntimeEnvironment,
    pub authority: Option<Authority>,
}

impl Program {
    pub fn is_shell(&self) -> bool {
        matches!(
            (self.source, self.path.as_str()),
            (ImageSource::Namespace, "/system/bin/sh")
        )
    }

    pub fn resolve(name: &str, cwd: &CanonicalPath) -> Result<Self, Error> {
        let (source, path) = if name.contains('/') {
            (
                ImageSource::Namespace,
                cwd.resolve(name)?.as_str()?.to_owned(),
            )
        } else if matches!(name, "mount" | "umount") {
            (ImageSource::Environment, format!("bin/{name}"))
        } else {
            (
                ImageSource::Namespace,
                CanonicalPath::root()
                    .resolve(&format!("/system/bin/{name}"))?
                    .as_str()?
                    .to_owned(),
            )
        };
        let environment = if source == ImageSource::Environment
            || path.starts_with("/boot/")
            || matches!(path.as_str(), "/bin/mount" | "/bin/umount")
        {
            RuntimeEnvironment::Recovery
        } else {
            RuntimeEnvironment::System
        };
        // Authority follows the selected trusted program, never its basename.
        let authority = match (source, path.as_str()) {
            (ImageSource::Environment, "bin/mount" | "bin/umount")
            | (
                ImageSource::Namespace,
                "/boot/bin/mount" | "/boot/bin/umount" | "/bin/mount" | "/bin/umount"
                | "/system/bin/mount" | "/system/bin/umount",
            ) => Some(Authority::Filesystem),
            (ImageSource::Namespace, "/system/bin/ps") => Some(Authority::Tasks),
            (ImageSource::Namespace, "/system/bin/handle") => Some(Authority::Handles),
            (ImageSource::Namespace, "/system/bin/free") => Some(Authority::Memory),
            (ImageSource::Namespace, "/system/bin/top") => Some(Authority::Activity),
            (ImageSource::Namespace, "/system/bin/vmm") => Some(Authority::VirtualMachines),
            _ => None,
        };
        Ok(Self {
            path,
            source,
            environment,
            authority,
        })
    }
}

#[cfg(test)]
#[path = "../tests/program.rs"]
mod tests;
