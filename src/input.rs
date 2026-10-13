// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Demand-driven redirected input. Reading ahead into a pipe would advance a
//! shared file description even when the child never reads, or reads one byte.
//! Only connector admission runs on a thread; the reactor owns all file offsets.

use crate::{Error, reactor::Source};
use hyper_os::capability_channel::CapabilityChannel;
use hyper_os::handle::{ByteChannelObject, CapabilityChannelObject, OwnedHandle};
use hyper_os::stream::{self, Header};
use hyper_os::wait::{ObjectSignals, WaitItem, wait_many};
use hyper_os::{Error as OsError, Status};
use hyper_shell::interpreter::io::Port;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub struct Input {
    pub owner: usize,
    port: Port,
    connector: CapabilityChannel,
    wake: OwnedHandle<ByteChannelObject>,
    arrivals: mpsc::Receiver<stream::Accepted>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    sessions: Vec<Session>,
}

impl Input {
    pub fn new(owner: usize, port: Port) -> Result<Self, Error> {
        let (connector, listener) = CapabilityChannel::create()?;
        let (wake_tx, wake) = hyper_os::channel::create_pair()?;
        let (send, arrivals) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::Builder::new()
            .name("sh-input-admit".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match stream::accept(&listener) {
                        Ok(accepted) => {
                            // A full bounded mailbox rejects this connection by
                            // closing it. No admission thread can stall shutdown.
                            let _ = send.try_send(accepted);
                            let _ = wake_tx.as_byte_channel().try_send(&[1]);
                        }
                        Err(OsError::Status(Status::PEER_CLOSED)) => break,
                        Err(_) => continue,
                    }
                }
            })
            .map_err(|e| Error::Message(format!("input admission: {e}")))?;
        Ok(Self {
            owner,
            port,
            connector,
            wake,
            arrivals,
            stop,
            worker: Some(worker),
            sessions: vec![],
        })
    }
    pub fn connector(&self) -> hyper_os::Result<OwnedHandle<CapabilityChannelObject>> {
        self.connector
            .as_handle_ref()
            .duplicate(stream::CONNECTOR_RIGHTS)
    }
    pub fn finish(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        self.stop.store(true, Ordering::Release);
        // Wake an indefinite receive. If the worker observes stop first, its
        // listener closes and the sender sees PEER_CLOSED instead. No timeout
        // or polling delay is added to every redirected command's exit.
        while let Err(OsError::Status(Status::WOULD_BLOCK)) =
            self.connector.try_send(b"stop", &mut [])
        {
            if wait_many(
                &[WaitItem::new(
                    self.connector.as_handle_ref(),
                    ObjectSignals::<CapabilityChannelObject>::PEER_RECEIVING
                        .union(ObjectSignals::<CapabilityChannelObject>::PEER_CLOSED),
                )],
                hyper_os::DEADLINE_INFINITE,
            )
            .is_err()
            {
                break;
            }
        }
        let _ = worker.join();
        self.sessions.clear();
        while self.arrivals.try_recv().is_ok() {}
    }
    pub fn poll(&mut self) -> Result<bool, Error> {
        if self.worker.is_none() {
            return Ok(false);
        }
        let mut wake = [0; hyper_os::channel::MAX_MESSAGE_BYTES];
        while self.wake.as_byte_channel().try_receive(&mut wake).is_ok() {}
        let mut progress = false;
        while let Ok(accepted) = self.arrivals.try_recv() {
            progress = true;
            if self.sessions.len() < 64 {
                let status = if accepted.access == stream::ACCESS_READ {
                    0
                } else {
                    Status::ACCESS_DENIED.as_raw()
                };
                self.sessions.push(Session {
                    channel: accepted.channel,
                    readable: status == 0,
                    closed: false,
                    pending: Some(
                        Header::response(stream::INFO, 0, 0, 0, status)
                            .encode()
                            .to_vec(),
                    ),
                });
            }
        }
        for session in &mut self.sessions {
            progress |= session.poll(&self.port)?;
        }
        self.sessions.retain(|s| !s.closed);
        Ok(progress)
    }
    pub fn waits<'a>(&'a self, waits: &mut Vec<Source<'a>>) {
        if self.worker.is_none() {
            return;
        }
        waits.push(Source::Channel(
            self.wake.as_handle_ref(),
            ObjectSignals::<ByteChannelObject>::READABLE,
        ));
        for s in &self.sessions {
            let signal = if s.pending.is_some() {
                ObjectSignals::<ByteChannelObject>::WRITABLE
            } else {
                ObjectSignals::<ByteChannelObject>::READABLE
            };
            waits.push(Source::Channel(
                s.channel.as_handle_ref(),
                signal.union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
            ));
        }
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        self.finish();
    }
}

struct Session {
    channel: OwnedHandle<ByteChannelObject>,
    readable: bool,
    closed: bool,
    pending: Option<Vec<u8>>,
}
impl Session {
    fn poll(&mut self, port: &Port) -> Result<bool, Error> {
        if self.pending.is_none() {
            let mut bytes = [0; stream::MAX_FRAME];
            let count = match self.channel.as_byte_channel().try_receive(&mut bytes) {
                Ok(n) => n,
                Err(OsError::Status(Status::WOULD_BLOCK)) => return Ok(false),
                Err(OsError::Status(Status::PEER_CLOSED)) => {
                    self.closed = true;
                    return Ok(true);
                }
                Err(e) => return Err(e.into()),
            };
            let Ok(h) = Header::decode(&bytes[..count]) else {
                self.closed = true;
                return Ok(true);
            };
            let mut payload = Vec::new();
            let (flags, status) = if !self.readable {
                (0, Status::ACCESS_DENIED.as_raw())
            } else if count != stream::HEADER_BYTES || h.flags != 0 {
                (0, Status::INVALID_ARGUMENT.as_raw())
            } else {
                match h.operation {
                    stream::READ if h.value > 0 && h.value <= stream::MAX_DATA as u64 => {
                        payload.resize(h.value as usize, 0);
                        match port.read(&mut payload) {
                            Ok(n) => {
                                payload.truncate(n);
                                (if n == 0 { stream::EOF } else { stream::DATA }, 0)
                            }
                            Err(_) => {
                                payload.clear();
                                (0, Status::IO_ERROR.as_raw())
                            }
                        }
                    }
                    stream::CANCEL | stream::FENCE | stream::INFO => (0, 0),
                    _ => (0, Status::NOT_SUPPORTED.as_raw()),
                }
            };
            let mut reply =
                Header::response(h.operation, h.id, flags, payload.len() as u64, status)
                    .encode()
                    .to_vec();
            reply.extend(payload);
            self.pending = Some(reply);
        }
        match self
            .channel
            .as_byte_channel()
            .try_send(self.pending.as_deref().ok_or(Error::Protocol)?)
        {
            Ok(()) => {
                self.pending = None;
                Ok(true)
            }
            Err(OsError::Status(Status::WOULD_BLOCK)) => Ok(false),
            Err(OsError::Status(Status::PEER_CLOSED)) => {
                self.closed = true;
                Ok(true)
            }
            Err(e) => Err(e.into()),
        }
    }
}
