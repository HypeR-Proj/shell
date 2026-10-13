// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Bounded file/terminal adapters. Pipeline edges themselves connect children.
use crate::Error;
use crate::reactor::Source;
use hyper_os::channel;
use hyper_os::handle::{ByteChannelObject, OwnedHandle};
use hyper_os::wait::ObjectSignals;
use hyper_os::{Error as OsError, Status};
use hyper_shell::interpreter::io::Port;

type Channel = OwnedHandle<ByteChannelObject>;
pub enum Endpoint {
    Owned(Channel),
    Data(Port),
}
impl Endpoint {
    fn channel(&self) -> Option<&Channel> {
        match self {
            Self::Owned(c) => Some(c),
            Self::Data(_) => None,
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
            let file_source = matches!(source, Endpoint::Data(_));
            let count = match source {
                Endpoint::Data(port) => port.read(&mut self.buffer).map_err(Error::Message)?,
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
            Endpoint::Data(port) => port
                .write(&self.buffer[..self.pending])
                .map_err(Error::Message)?,
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
    pub fn wait_item(&self) -> Option<Source<'_>> {
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
        Some(Source::Channel(
            channel.as_handle_ref(),
            signal.union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
        ))
    }
}
