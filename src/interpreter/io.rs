// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Redirection aliases share one open description, including its file offset.
//! This table is shell policy; it does not create a POSIX descriptor ABI.

use super::Result;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::rc::Rc;

pub trait Descriptor {
    fn read(&self, bytes: &mut [u8]) -> Result<usize>;
    fn write(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Clone)]
pub enum Port {
    Standard(u8),
    File(Rc<RefCell<std::fs::File>>),
    Memory(Rc<RefCell<Cursor<Vec<u8>>>>),
    Capture(Rc<RefCell<Vec<u8>>>),
    Remote(Rc<dyn Descriptor>),
    Closed,
    /// Wired by the process launcher before redirections are applied.
    Pipe {
        edge: usize,
        reading: bool,
    },
}

impl Port {
    pub fn memory(bytes: Vec<u8>) -> Self {
        Self::Memory(Rc::new(RefCell::new(Cursor::new(bytes))))
    }
    pub fn read(&self, bytes: &mut [u8]) -> Result<usize> {
        let result = match self {
            Self::Standard(0) => std::io::stdin().read(bytes),
            Self::File(file) => file.borrow_mut().read(bytes),
            Self::Memory(cursor) => cursor.borrow_mut().read(bytes),
            Self::Remote(remote) => return remote.read(bytes),
            Self::Closed => return Err("input descriptor is closed".into()),
            _ => return Err("descriptor is not readable".into()),
        };
        result.map_err(|e| e.to_string())
    }
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let result = match self {
            Self::Standard(1) => {
                let mut out = std::io::stdout().lock();
                out.write_all(bytes).and_then(|()| out.flush())
            }
            Self::Standard(2) => {
                let mut out = std::io::stderr().lock();
                out.write_all(bytes).and_then(|()| out.flush())
            }
            Self::File(file) => file.borrow_mut().write_all(bytes),
            Self::Remote(remote) => return remote.write(bytes),
            Self::Capture(buffer) => {
                let mut buffer = buffer.borrow_mut();
                if buffer.len().saturating_add(bytes.len()) > crate::syntax::MAX_SCRIPT_BYTES {
                    return Err("command substitution output limit exceeded".into());
                }
                buffer.extend_from_slice(bytes);
                Ok(())
            }
            Self::Closed => return Err("output descriptor is closed".into()),
            _ => return Err("descriptor is not writable".into()),
        };
        result.map_err(|e| e.to_string())
    }
    pub fn aliases(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Standard(a), Self::Standard(b)) => a == b,
            (Self::File(a), Self::File(b)) => Rc::ptr_eq(a, b),
            (Self::Memory(a), Self::Memory(b)) => Rc::ptr_eq(a, b),
            (Self::Capture(a), Self::Capture(b)) => Rc::ptr_eq(a, b),
            (Self::Remote(a), Self::Remote(b)) => Rc::ptr_eq(a, b),
            (
                Self::Pipe {
                    edge: a,
                    reading: ar,
                },
                Self::Pipe {
                    edge: b,
                    reading: br,
                },
            ) => a == b && ar == br,
            (Self::Closed, Self::Closed) => true,
            _ => false,
        }
    }
}

#[derive(Clone)]
pub struct Streams(pub BTreeMap<u8, Port>);

impl Default for Streams {
    fn default() -> Self {
        Self((0..3).map(|fd| (fd, Port::Standard(fd))).collect())
    }
}

impl Streams {
    pub fn get(&self, fd: u8) -> Port {
        self.0.get(&fd).cloned().unwrap_or(Port::Closed)
    }
    pub fn set(&mut self, fd: u8, port: Port) {
        self.0.insert(fd, port);
    }
    pub fn write(&self, fd: u8, text: &str) -> Result<()> {
        self.get(fd).write(text.as_bytes())
    }
    pub fn duplicate(&mut self, fd: u8, source: u8) -> Result<()> {
        let port = self.get(source);
        if matches!(port, Port::Closed) {
            return Err(format!("{source}: descriptor is closed"));
        }
        self.set(fd, port);
        Ok(())
    }
}
