// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Shell-local descriptor inheritance without a foreign fd ABI. Workers make
//! request-sized operations against the parent's open descriptions, so aliases
//! keep one offset and an idle reader cannot consume a following command's data.

use crate::reactor::Source;
use hyper_os::handle::{ByteChannelObject, OwnedHandle};
use hyper_os::startup::StartupPurpose;
use hyper_os::wait::ObjectSignals;
use hyper_os::{Error, Status};
use hyper_shell::interpreter::io::{Descriptor, Streams};
use std::rc::Rc;

pub const CONNECTOR: StartupPurpose<ByteChannelObject> = StartupPurpose::new(0x800a_0002);
const MAX_DATA: usize = hyper_os::channel::MAX_MESSAGE_BYTES - 8;

pub struct Remote {
    pub channel: Rc<OwnedHandle<ByteChannelObject>>,
    pub fd: u8,
}

impl Remote {
    fn request(&self, operation: u8, bytes: &[u8], count: usize) -> Result<Vec<u8>, String> {
        let mut request = Vec::with_capacity(bytes.len() + 8);
        request.extend_from_slice(&[operation, self.fd, 0, 0]);
        request.extend_from_slice(&(count as u32).to_le_bytes());
        request.extend_from_slice(bytes);
        self.channel
            .as_byte_channel()
            .send(&request)
            .map_err(|e| format!("descriptor request: {e:?}"))?;
        let mut response = vec![0; hyper_os::channel::MAX_MESSAGE_BYTES];
        let n = self
            .channel
            .as_byte_channel()
            .receive(&mut response)
            .map_err(|e| format!("descriptor response: {e:?}"))?;
        response.truncate(n);
        if response.first() != Some(&0) {
            return Err(
                String::from_utf8_lossy(response.get(1..).unwrap_or_default()).into_owned(),
            );
        }
        Ok(response[1..].to_vec())
    }
}
impl Descriptor for Remote {
    fn read(&self, bytes: &mut [u8]) -> Result<usize, String> {
        let response = self.request(0, &[], bytes.len().min(MAX_DATA))?;
        if response.len() > bytes.len() {
            return Err("oversized descriptor response".into());
        }
        bytes[..response.len()].copy_from_slice(&response);
        Ok(response.len())
    }
    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        for chunk in bytes.chunks(MAX_DATA) {
            self.request(1, chunk, chunk.len())?;
        }
        Ok(())
    }
}

pub struct Server {
    pub owner: usize,
    channel: OwnedHandle<ByteChannelObject>,
    streams: Streams,
    pending: Option<Vec<u8>>,
    closed: bool,
}
impl Server {
    pub fn new(owner: usize, channel: OwnedHandle<ByteChannelObject>, streams: Streams) -> Self {
        Self {
            owner,
            channel,
            streams,
            pending: None,
            closed: false,
        }
    }
    pub fn finish(&mut self) {
        self.closed = true;
    }
    pub fn poll(&mut self) -> Result<bool, crate::Error> {
        if self.closed {
            return Ok(false);
        }
        if self.pending.is_none() {
            let mut request = [0; hyper_os::channel::MAX_MESSAGE_BYTES];
            let n = match self.channel.as_byte_channel().try_receive(&mut request) {
                Ok(n) => n,
                Err(Error::Status(Status::WOULD_BLOCK)) => return Ok(false),
                Err(Error::Status(Status::PEER_CLOSED)) => {
                    self.closed = true;
                    return Ok(true);
                }
                Err(error) => return Err(error.into()),
            };
            let result = self.operation(&request[..n]);
            let mut response = vec![u8::from(result.is_err())];
            match result {
                Ok(bytes) => response.extend(bytes),
                Err(error) => response.extend(error.bytes().take(MAX_DATA)),
            }
            self.pending = Some(response);
        }
        match self
            .channel
            .as_byte_channel()
            .try_send(self.pending.as_deref().ok_or(crate::Error::Protocol)?)
        {
            Ok(()) => {
                self.pending = None;
                Ok(true)
            }
            Err(Error::Status(Status::WOULD_BLOCK)) => Ok(false),
            Err(Error::Status(Status::PEER_CLOSED)) => {
                self.closed = true;
                Ok(true)
            }
            Err(error) => Err(error.into()),
        }
    }
    fn operation(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        if request.len() < 8 || request[1] < 3 || request[1] >= 64 {
            return Err("invalid descriptor request".into());
        }
        let count =
            u32::from_le_bytes(request[4..8].try_into().map_err(|_| "invalid count")?) as usize;
        if count > MAX_DATA {
            return Err("descriptor operation too large".into());
        }
        let port = self.streams.get(request[1]);
        match request[0] {
            0 if request.len() == 8 => {
                let mut bytes = vec![0; count];
                let n = port.read(&mut bytes)?;
                bytes.truncate(n);
                Ok(bytes)
            }
            1 if request.len() == count + 8 => {
                port.write(&request[8..])?;
                Ok(vec![])
            }
            _ => Err("invalid descriptor operation".into()),
        }
    }
    pub fn wait_item(&self) -> Option<Source<'_>> {
        if self.closed {
            return None;
        }
        let signals = if self.pending.is_some() {
            ObjectSignals::<ByteChannelObject>::WRITABLE
        } else {
            ObjectSignals::<ByteChannelObject>::READABLE
        };
        Some(Source::Channel(
            self.channel.as_handle_ref(),
            signals.union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
        ))
    }
}
