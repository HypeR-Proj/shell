// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Bounded file/terminal adapters. Pipeline edges themselves connect children.
use crate::Error;
use hyper_os::channel;
use hyper_os::handle::{ByteChannelObject, OwnedHandle};
use hyper_os::wait::{ObjectSignals, WaitItem};
use hyper_os::{Error as OsError, Status};
use std::fs::File;
use std::io::{Read, Write};

type Channel = OwnedHandle<ByteChannelObject>;
pub enum Endpoint {
    Owned(Channel),
    Terminal,
    File(File),
}
impl Endpoint {
    fn channel(&self) -> Option<&Channel> {
        match self {
            Self::Owned(c) => Some(c),
            Self::Terminal => None,
            Self::File(_) => None,
        }
    }
}
pub struct Route {
    source: Option<Endpoint>,
    destination: Option<Endpoint>,
    buffer: Vec<u8>,
    pending: usize,
    pub producer: Option<usize>,
    pub consumer: Option<usize>,
}
impl Route {
    pub fn new(
        source: Endpoint,
        destination: Endpoint,
        producer: Option<usize>,
        consumer: Option<usize>,
    ) -> Self {
        Self {
            source: Some(source),
            destination: Some(destination),
            buffer: vec![0; channel::MAX_MESSAGE_BYTES],
            pending: 0,
            producer,
            consumer,
        }
    }
    pub fn finish(&mut self) {
        self.source = None;
        self.destination = None;
        self.pending = 0;
    }
    pub fn finished(&self) -> bool {
        self.destination.is_none()
    }
    pub fn has_pending(&self) -> bool {
        self.pending != 0
    }
    pub fn poll(&mut self) -> Result<bool, Error> {
        if self.finished() {
            return Ok(false);
        }
        if self.pending == 0 {
            let Some(source) = self.source.as_mut() else {
                self.finish();
                return Ok(true);
            };
            let file_source = matches!(source, Endpoint::File(_));
            let count = match source {
                Endpoint::File(file) => file.read(&mut self.buffer).map_err(Error::Io)?,
                source => match source
                    .channel()
                    .ok_or(Error::Protocol)?
                    .as_byte_channel()
                    .try_receive(&mut self.buffer)
                {
                    Ok(count) => count,
                    Err(OsError::Status(Status::WOULD_BLOCK)) => return Ok(false),
                    Err(OsError::Status(Status::PEER_CLOSED)) => {
                        self.finish();
                        return Ok(true);
                    }
                    Err(error) => return Err(error.into()),
                },
            };
            if count == 0 {
                if file_source {
                    self.finish();
                }
                return Ok(true);
            }
            self.pending = count;
        }
        match self.destination.as_mut().ok_or(Error::Protocol)? {
            Endpoint::File(file) => file
                .write_all(&self.buffer[..self.pending])
                .map_err(Error::Io)?,
            Endpoint::Terminal => {
                let mut output = std::io::stdout().lock();
                output
                    .write_all(&self.buffer[..self.pending])
                    .map_err(Error::Io)?;
                // A relayed child prompt need not end in LF. Do not retain it
                // in this shell's line buffer while the child waits for input.
                output.flush().map_err(Error::Io)?;
            }
            destination => match destination
                .channel()
                .ok_or(Error::Protocol)?
                .as_byte_channel()
                .try_send(&self.buffer[..self.pending])
            {
                Ok(()) => {}
                Err(OsError::Status(Status::WOULD_BLOCK)) => return Ok(false),
                Err(OsError::Status(Status::PEER_CLOSED)) => {
                    self.finish();
                    return Ok(true);
                }
                Err(error) => return Err(error.into()),
            },
        }
        self.pending = 0;
        Ok(true)
    }
    pub fn wait_item(&self) -> Option<WaitItem<'_>> {
        let endpoint = if self.pending == 0 {
            self.source.as_ref()?
        } else {
            self.destination.as_ref()?
        };
        let channel = endpoint.channel()?;
        let signal = if self.pending == 0 {
            ObjectSignals::<ByteChannelObject>::READABLE
        } else {
            ObjectSignals::<ByteChannelObject>::WRITABLE
        };
        Some(WaitItem::new(
            channel.as_handle_ref(),
            signal.union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
        ))
    }
}
