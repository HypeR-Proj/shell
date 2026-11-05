// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Cooperative shell safe points and stdin waits, with one owner per read.

use crate::{
    Error,
    launch::NativeBackend,
    reactor::{self, Source},
};
use hyper_os::handle::ByteChannelObject;
use hyper_os::stream::{ProcessInput, ReadResult};
use hyper_os::wait::{ObjectSignals, wait_many};
use hyper_os::{Error as OsError, Status};

impl NativeBackend {
    pub fn collect_jobs(&mut self) -> Result<(), Error> {
        let mut index = 0;
        while index < self.jobs.len() {
            if self.jobs[index].can_retire()? {
                self.jobs.swap_remove(index);
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    pub fn poll_control(&mut self) -> Result<bool, Error> {
        let mut progressed = false;
        if let Some(client) = self.control.as_mut() {
            match client.try_receive() {
                Ok(Some(_)) => {
                    self.exit_requested = true;
                    progressed = true;
                }
                Ok(None) => {}
                Err(OsError::Status(Status::PEER_CLOSED)) => {
                    self.control = None;
                }
                Err(error) => {
                    self.control = None;
                    eprintln!("sh: parent control unavailable: {error:?}");
                }
            }
        }
        if let Some(terminal) = self.authorities.terminal.as_mut()
            && let Some(wait) = terminal.event_wait_item()
        {
            match wait_many(&[wait], 0) {
                Ok(_) => {
                    match terminal.take_events() {
                        Ok(events) => {
                            if events.0 & hyper_terminal::EventFlags::TERMINATE != 0 {
                                self.cancel_requested = true;
                            }
                        }
                        Err(OsError::Status(Status::PEER_CLOSED)) => {
                            // A lost terminal is not a termination request.
                            // Keep supervising children while they observe
                            // stream closure and use the session cleanup grace.
                            self.authorities.terminal = None;
                        }
                        Err(error) => return Err(error.into()),
                    }
                    progressed = true;
                }
                Err(OsError::Status(Status::TIMED_OUT)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        for job in &mut self.jobs {
            progressed |= job.poll()?;
        }
        Ok(progressed)
    }

    pub fn control_waits<'a>(&'a self, waits: &mut Vec<Source<'a>>) {
        if let Some(client) = self.control.as_ref()
            && let Some(handle) = client.as_handle_ref()
        {
            waits.push(Source::Channel(handle, client.signals()));
        }
        if let Some(terminal) = self.authorities.terminal.as_ref()
            && let Some(handle) = terminal.event_handle()
        {
            waits.push(Source::Channel(
                handle,
                ObjectSignals::<ByteChannelObject>::READABLE
                    .union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
            ));
        }
        for job in &self.jobs {
            job.waits(waits);
        }
    }

    pub fn read_standard(&mut self, output: &mut [u8]) -> Result<Option<usize>, Error> {
        if output.is_empty() {
            return Ok(Some(0));
        }
        self.collect_jobs()?;
        self.poll_control()?;
        if self.exit_requested || self.cancel_requested {
            return Ok(None);
        }
        if hyper_rt::process::stdin_stream()?.is_none() {
            loop {
                if !self.input_buffer.is_empty() {
                    let count = output.len().min(self.input_buffer.len());
                    for byte in &mut output[..count] {
                        *byte = self.input_buffer.pop_front().ok_or(Error::Protocol)?;
                    }
                    return Ok(Some(count));
                }
                self.poll_control()?;
                if self.exit_requested || self.cancel_requested {
                    return Ok(None);
                }
                let input = hyper_rt::process::stdin()?;
                if self.pipe_scratch.is_empty() {
                    // ByteChannel reads consume whole messages. Keep their
                    // maximum payload off the stack, including on stream-only
                    // calls, and allocate nothing while draining saved bytes.
                    self.pipe_scratch
                        .try_reserve_exact(hyper_os::channel::MAX_MESSAGE_BYTES)
                        .map_err(|_| OsError::Status(Status::NO_MEMORY))?;
                    self.pipe_scratch
                        .resize(hyper_os::channel::MAX_MESSAGE_BYTES, 0);
                }
                // Reserve saved-byte ownership before consuming the message;
                // allocation failure must not discard a committed pipe read.
                self.input_buffer
                    .try_reserve(hyper_os::channel::MAX_MESSAGE_BYTES)
                    .map_err(|_| OsError::Status(Status::NO_MEMORY))?;
                match input.as_byte_channel().try_receive(&mut self.pipe_scratch) {
                    Ok(count) => {
                        self.input_buffer.extend(&self.pipe_scratch[..count]);
                    }
                    Err(OsError::Status(Status::PEER_CLOSED)) => return Ok(Some(0)),
                    Err(OsError::Status(Status::WOULD_BLOCK)) => {
                        let mut waits = Vec::new();
                        self.control_waits(&mut waits);
                        waits.push(Source::Channel(
                            input.as_handle_ref(),
                            ObjectSignals::<ByteChannelObject>::READABLE
                                .union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
                        ));
                        reactor::wait(&waits)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let mut input = ProcessInput::acquire()?;
        input.start_read(output.len(), hyper_os::DEADLINE_INFINITE)?;
        let mut cancelled = false;
        loop {
            self.poll_control()?;
            let stopping = self.exit_requested || self.cancel_requested;
            // A committed reply wins over cancellation, including peer closure
            // after its final reply. Never overwrite it with a failed CANCEL.
            let mut completed = input.try_complete(output)?;
            if completed.is_none() && stopping && !cancelled {
                match input.cancel() {
                    Ok(()) => cancelled = true,
                    Err(error) => {
                        completed = input.try_complete(output)?;
                        if completed.is_none() {
                            return Err(error.into());
                        }
                    }
                }
            }
            if let Some(result) = completed {
                if result == ReadResult::ConfigurationChanged && !stopping {
                    // Completion releases the previous request before restart;
                    // repeated settings changes never grow the call stack.
                    input.start_read(output.len(), hyper_os::DEADLINE_INFINITE)?;
                    continue;
                }
                return match result {
                    ReadResult::Data(count) => Ok(Some(count)),
                    ReadResult::Eof => Ok(Some(0)),
                    ReadResult::Cancelled if stopping => Ok(None),
                    ReadResult::ConfigurationChanged if stopping => Ok(None),
                    _ => Err(Error::Protocol),
                };
            }
            let mut waits = Vec::new();
            self.control_waits(&mut waits);
            waits.push(Source::Channel(
                input.as_handle_ref(),
                ObjectSignals::<ByteChannelObject>::READABLE
                    .union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
            ));
            reactor::wait(&waits)?;
        }
    }
}
