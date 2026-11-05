// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Shell-owned job admission; lifecycle policy stays in the main reactor.

use crate::{Error, reactor::Source};
use hyper_os::handle::{ByteChannelObject, OwnedHandle};
use hyper_os::wait::ObjectSignals;
use hyper_os::{Error as OsError, Status};
use hyper_service::process::job;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub struct Job {
    pub registry: job::Registry,
    client: job::Client,
    broadcaster: job::Broadcaster,
    broadcasts: job::Requests,
    wake: OwnedHandle<ByteChannelObject>,
    arrivals: mpsc::Receiver<hyper_os::Result<job::Arrival>>,
    stopping: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Job {
    pub fn new() -> Result<Self, Error> {
        let (listener, client) = job::Client::create()?;
        let (broadcasts, broadcaster) = job::Broadcaster::create()?;
        let (wake_tx, wake) = hyper_os::channel::create_pair()?;
        let (sender, arrivals) = mpsc::sync_channel(16);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let worker = std::thread::Builder::new()
            .name("sh-job-admit".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match job::accept(&listener) {
                        Ok(arrival) => {
                            // On overflow, dropping the private acknowledgment
                            // rejects registration. Broadcasts use another channel.
                            if sender.try_send(Ok(arrival)).is_ok() {
                                let _ = wake_tx.as_byte_channel().try_send(&[1]);
                            }
                        }
                        Err(OsError::Status(Status::PEER_CLOSED)) => break,
                        Err(OsError::InvalidResponse | OsError::ReceiveRejected(_)) => {}
                        Err(error) => {
                            let _ = sender.try_send(Err(error));
                            let _ = wake_tx.as_byte_channel().try_send(&[1]);
                            break;
                        }
                    }
                }
            })
            .map_err(Error::Io)?;
        Ok(Self {
            registry: job::Registry::new(),
            client,
            broadcaster,
            broadcasts,
            wake,
            arrivals,
            stopping,
            worker: Some(worker),
        })
    }

    pub fn client(&self) -> hyper_os::Result<job::Client> {
        self.client.duplicate()
    }

    pub fn broadcaster(&self) -> hyper_os::Result<job::Broadcaster> {
        self.broadcaster.duplicate()
    }

    pub fn member_count(&self) -> usize {
        self.registry.member_count()
    }

    /// Call only after this job's local launch transactions have completed.
    /// Standard parents stay live until descendant registration is acknowledged.
    /// The admission rendezvous fences a receive already completed by the worker
    /// but not yet posted to `arrivals`; the second poll retains those children.
    /// Merely keeping a registration capability does not keep an empty job alive.
    pub fn can_retire(&mut self) -> Result<bool, Error> {
        self.poll()?;
        if self.member_count() != 0 {
            return Ok(false);
        }
        self.client.wake_admission()?;
        self.poll()?;
        Ok(self.registry.is_empty())
    }

    pub fn poll(&mut self) -> Result<bool, Error> {
        let mut progressed = false;
        let mut byte = [0; 1];
        while self.wake.as_byte_channel().try_receive(&mut byte).is_ok() {}
        for _ in 0..16 {
            let arrival = match self.arrivals.try_recv() {
                Ok(arrival) => arrival,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Error::Message("job admission worker stopped".into()));
                }
            };
            self.registry.admit(arrival?);
            progressed = true;
        }
        if self.broadcasts.poll()? {
            self.registry.request_terminate();
            progressed = true;
        }
        Ok(self.registry.poll()? || progressed)
    }

    pub fn waits<'a>(&'a self, waits: &mut Vec<Source<'a>>) {
        waits.push(Source::Channel(
            self.wake.as_handle_ref(),
            ObjectSignals::<ByteChannelObject>::READABLE
                .union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
        ));
        if let Some((handle, signals)) = self.broadcasts.watch() {
            waits.push(Source::Channel(handle, signals));
        }
        waits.extend(self.registry.watches().map(|(h, s)| Source::Channel(h, s)));
        waits.extend(self.registry.observers().map(Source::Process));
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // The worker owns the only listener handle. If it sees stop before
            // this send, dropping that listener wakes the send with PEER_CLOSED.
            // Otherwise the empty request wakes its receive, then it sees stop.
            // The worker never waits for an arrival consumer or client reply.
            let _ = self.client.wake_admission();
            let _ = worker.join();
        }
    }
}
