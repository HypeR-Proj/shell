// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Capability-preserving pipeline construction and foreground supervision.
use super::capabilities::{ChildChannels, prepare_command};
use super::{CommandAuthorities, Error, WORKING_DIRECTORY_RIGHTS};
use crate::reactor::{self, Source};
use crate::route;
use hyper_os::channel;
use hyper_os::handle::{
    ByteChannelObject, CapabilityChannelObject, OwnedHandle, ProcessObject, Rights, RightsOffer,
};
use hyper_os::startup;
use hyper_os::task::ProcessTermination;
use hyper_service::process::{control, job};
use hyper_service::stdio::{Inherited, StandardStream};
use hyper_shell::interpreter::io::Port;
use hyper_shell::interpreter::{Backend, Invocation, Stage};

// Private shell-to-shell state transfer. Large scripts do not consume argv,
// environment space, a temporary filesystem file, or the command's stdin.
pub(super) const WORKER_STATE: startup::StartupPurpose<ByteChannelObject> =
    startup::StartupPurpose::new(0x800a_0001);

#[derive(Default)]
struct Adapters {
    routes: Vec<route::Route>,
    inputs: Vec<crate::input::Input>,
}

pub(super) struct NativeBackend {
    pub authorities: CommandAuthorities,
    pub input: Inherited,
    pub output: Inherited,
    pub error: Inherited,
    pub terminal_stdio: [bool; 3],
    pub read_settings: Option<hyper_terminal::Settings>,
    pub control: Option<control::Client>,
    pub inherited_job: Option<job::Client>,
    pub jobs: Vec<crate::jobs::Job>,
    pub exit_requested: bool,
    pub cancel_requested: bool,
    pub input_buffer: std::collections::VecDeque<u8>,
    pub pipe_scratch: Vec<u8>,
    pub inherited_broadcast: Option<job::Broadcaster>,
}

impl Backend for NativeBackend {
    fn checkpoint(&mut self) -> Result<bool, String> {
        self.poll_control().map_err(|e| e.to_string())?;
        Ok(self.exit_requested || self.cancel_requested)
    }

    fn read(&mut self, port: &Port, bytes: &mut [u8]) -> Result<usize, String> {
        if matches!(port, Port::Standard(0)) {
            self.read_standard(bytes)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "operation cancelled".into())
        } else {
            port.read(bytes)
        }
    }

    fn canonical_input(&mut self, enabled: bool) -> Result<(), String> {
        // The pipeline's final consumer owns terminal control even when stdin
        // is a pipe (for example cat | less). A read from that pipe must not
        // reconfigure the shared lease and cancel an upstream terminal reader.
        if !self.terminal_stdio[0] {
            return Ok(());
        }
        let Some(terminal) = self.authorities.terminal.as_mut() else {
            return Ok(());
        };
        if enabled {
            let settings = terminal
                .settings()
                .map_err(|e| format!("terminal settings: {e:?}"))?;
            terminal
                .set_settings(hyper_terminal::Settings::default())
                .map_err(|e| format!("terminal canonical input: {e:?}"))?;
            self.read_settings = Some(settings);
        } else if let Some(settings) = self.read_settings.take() {
            terminal
                .set_settings(settings)
                .map_err(|e| format!("terminal restore: {e:?}"))?;
        }
        Ok(())
    }
    fn directory(&mut self, target: &str) -> Result<(), String> {
        let path = self
            .authorities
            .current_path
            .resolve(target)
            .map_err(|_| "invalid directory path")?;
        let text = path.as_str().map_err(|_| "invalid directory path")?;
        let directory = if text == "/" {
            self.authorities
                .root_directory
                .scope_at(&self.authorities.root_directory, WORKING_DIRECTORY_RIGHTS)
        } else {
            self.authorities
                .root_directory
                .open_directory(text, WORKING_DIRECTORY_RIGHTS)
        }
        .map_err(|e| format!("{text}: {e:?}"))?;
        self.authorities.current_directory = directory;
        self.authorities.current_path = path;
        Ok(())
    }

    fn pipeline(&mut self, stages: Vec<Stage>) -> Result<Vec<i32>, String> {
        let mut lease = None;
        let result = self.launch(stages, &mut lease);
        // No launch-local registry index remains borrowed after launch returns.
        let cleanup = self.collect_jobs();
        if let (Some(id), Some(terminal)) = (lease, self.authorities.terminal.as_mut()) {
            let cleanup = terminal.release_child(id);
            if result.is_ok() {
                cleanup.map_err(|e| format!("terminal release: {e:?}"))?;
            }
        }
        let result = result.map_err(|e| e.to_string())?;
        cleanup.map_err(|e| e.to_string())?;
        Ok(result)
    }
}

impl NativeBackend {
    fn launch(&mut self, stages: Vec<Stage>, lease: &mut Option<u64>) -> Result<Vec<i32>, Error> {
        let root_job = if self.inherited_job.is_none() {
            self.jobs.push(crate::jobs::Job::new()?);
            Some(self.jobs.len() - 1)
        } else {
            None
        };
        let job_client = match root_job {
            Some(index) => self.jobs[index].client()?,
            None => self
                .inherited_job
                .as_ref()
                .ok_or(Error::Protocol)?
                .duplicate()?,
        };
        let mut broadcaster = match root_job {
            Some(index) => self.jobs[index].broadcaster()?,
            None => self
                .inherited_broadcast
                .as_ref()
                .ok_or(Error::Protocol)?
                .duplicate()?,
        };
        let mut job_events = None;
        let mut controls = Vec::new();
        let mut request_receivers = Vec::new();
        let mut request_senders = Vec::new();
        let mut adapters = Adapters::default();
        let mut descriptors = Vec::new();
        let mut builders = Vec::new();
        let mut pipes = Vec::new();
        for _ in 1..stages.len() {
            pipes.push(channel::create_pair()?);
        }
        // One foreground lease belongs to the whole pipeline. Its final
        // consumer alone receives control, and remains the owner after an
        // upstream producer exits (cat FILE | less). Ordinary stream writers
        // do not need terminal-control authority.
        let lease_owner = stages.len().checked_sub(1);
        let mut client = if !stages.is_empty() && self.authorities.terminal.is_some() {
            let terminal = self.authorities.terminal.as_mut().ok_or(Error::Protocol)?;
            let prepared = terminal.prepare_job()?;
            *lease = Some(prepared.client.id);
            job_events = Some(prepared.events);
            Some(prepared.client)
        } else {
            None
        };
        for (index, stage) in stages.into_iter().enumerate() {
            let terminal_stream = client.as_ref().map(|c| &c.stream);
            let child_input = self.endpoint(
                stage.streams.get(0),
                0,
                index,
                &pipes,
                terminal_stream,
                &mut adapters,
            )?;
            let child_output = self.endpoint(
                stage.streams.get(1),
                1,
                index,
                &pipes,
                terminal_stream,
                &mut adapters,
            )?;
            let child_error = if stage.streams.get(2).aliases(&stage.streams.get(1)) {
                child_output.duplicate(StandardStream::Error)?
            } else {
                self.endpoint(
                    stage.streams.get(2),
                    2,
                    index,
                    &pipes,
                    terminal_stream,
                    &mut adapters,
                )?
            };
            let terminal = if lease_owner == Some(index) {
                client.take()
            } else {
                None
            };
            let mut descriptor_child = None;
            let (arguments, environment, payload) = match stage.invocation {
                Invocation::External {
                    arguments,
                    environment,
                } => (arguments, environment, None),
                Invocation::Shell(mut worker) => {
                    // Keep aliases of inherited standard streams local to the
                    // worker. In particular, a terminal reader must use the
                    // child's lease, never call back into the inactive parent.
                    worker.aliases = stage
                        .streams
                        .0
                        .iter()
                        .filter(|(fd, port)| **fd > 2 && !matches!(port, Port::Closed))
                        .filter_map(|(fd, port)| {
                            (0..3)
                                .find(|n| port.aliases(&stage.streams.get(*n)))
                                .map(|source| (*fd, source))
                        })
                        .collect();
                    worker.descriptors = stage
                        .streams
                        .0
                        .iter()
                        .filter(|(fd, port)| {
                            **fd > 2
                                && !matches!(port, Port::Closed)
                                && !worker.aliases.iter().any(|(alias, _)| alias == *fd)
                        })
                        .map(|(fd, _)| *fd)
                        .collect();
                    if !worker.descriptors.is_empty() {
                        let (parent, child) = channel::create_pair()?;
                        descriptors.push(crate::descriptor::Server::new(
                            index,
                            parent,
                            stage.streams.clone(),
                        ));
                        descriptor_child = Some(child);
                    }
                    let bytes =
                        serde_json::to_vec(&worker).map_err(|e| Error::Message(e.to_string()))?;
                    if bytes.len() > hyper_shell::syntax::MAX_SCRIPT_BYTES {
                        return Err(Error::Message("shell worker state limit exceeded".into()));
                    }
                    (
                        vec!["/bin/sh".into(), "--worker".into()],
                        worker.state.environment(),
                        Some(bytes),
                    )
                }
            };
            let builder = prepare_command(
                &arguments,
                &environment,
                &self.authorities,
                ChildChannels {
                    input: child_input,
                    output: child_output,
                    error: child_error,
                    terminal,
                },
            )?;
            let (supervisor, control_child) = control::prepare()?;
            builder
                .add_handle_move(
                    control_child,
                    control::STARTUP.as_raw(),
                    RightsOffer::Exact(control::CHILD_RIGHTS),
                )
                .map_err(|e| e.error())?;
            job_client.add_to(&builder)?;
            if hyper_shell::program::Program::resolve(&arguments[0], &self.authorities.current_path)
                .map_err(|_| Error::InvalidCommand)?
                .is_shell()
            {
                broadcaster.add_to(&builder)?;
            }
            controls.push(supervisor);
            let (receiver, sender) = job::Requests::create()?;
            request_receivers.push(receiver);
            request_senders.push(sender);
            if let Some(bytes) = payload {
                let (parent, child) = channel::create_pair()?;
                builder
                    .add_handle_move(
                        child,
                        WORKER_STATE.as_raw(),
                        RightsOffer::Exact(Rights::READ.union(Rights::WAIT)),
                    )
                    .map_err(|e| e.error())?;
                adapters.routes.push(route::Route::new(
                    route::Endpoint::Data(Port::memory(bytes)),
                    route::Endpoint::Owned(parent),
                    None,
                    Some(index),
                ));
            }
            if let Some(child) = descriptor_child {
                builder
                    .add_handle_move(
                        child,
                        crate::descriptor::CONNECTOR.as_raw(),
                        RightsOffer::Exact(Rights::READ.union(Rights::WRITE).union(Rights::WAIT)),
                    )
                    .map_err(|e| e.error())?;
            }
            builder.seal()?;
            builders.push(builder);
        }
        // Only children retain pipe edges once launched. Keeping a parent end
        // would prevent downstream EOF and can deadlock a finite pipeline.
        drop(pipes);
        let mut running = Running(Vec::new());
        for (builder, sender) in builders.into_iter().zip(request_senders) {
            running.0.push(builder.start().map_err(|e| e.error())?);
            let process = running.0.last().ok_or(Error::Protocol)?;
            let observer = process.as_process_supervisor().observe()?;
            match root_job {
                Some(index) => self.jobs[index].registry.register(observer, sender)?,
                None => job_client.register(observer, sender)?,
            }
        }
        if let (Some(id), Some(terminal)) = (*lease, self.authorities.terminal.as_mut()) {
            let observer = running
                .0
                .last()
                .ok_or(Error::Protocol)?
                .as_process_supervisor()
                .observe()?;
            terminal.bind_job(id, observer)?;
        }
        relay_pipeline(
            &running.0,
            &mut adapters.routes,
            &mut descriptors,
            &mut adapters.inputs,
            self,
            &mut controls,
            &mut request_receivers,
            job_events.as_mut(),
            &mut broadcaster,
            root_job,
        )
    }

    fn endpoint(
        &self,
        port: Port,
        fd: u8,
        index: usize,
        pipes: &[(
            OwnedHandle<ByteChannelObject>,
            OwnedHandle<ByteChannelObject>,
        )],
        terminal: Option<&OwnedHandle<CapabilityChannelObject>>,
        adapters: &mut Adapters,
    ) -> Result<Inherited, Error> {
        let rights = Rights::WAIT
            .union(Rights::DUPLICATE)
            .union(Rights::TRANSFER)
            .union(if fd == 0 { Rights::READ } else { Rights::WRITE });
        match port {
            Port::Pipe { edge, reading } => {
                let pair = pipes.get(edge).ok_or(Error::Protocol)?;
                let handle = if reading { &pair.1 } else { &pair.0 };
                Ok(Inherited::Pipe(handle.duplicate(rights)?))
            }
            Port::Standard(0) if fd == 0 => {
                if let Some(terminal) = terminal.filter(|_| self.terminal_stdio[0]) {
                    Ok(Inherited::Stream(
                        terminal.duplicate(hyper_os::stream::CONNECTOR_RIGHTS)?,
                    ))
                } else {
                    Ok(self.input.duplicate(StandardStream::Input)?)
                }
            }
            // A shell worker may inherit a pipe or redirected file under a
            // standard descriptor. Only replace actual terminal endpoints;
            // control ownership does not change data-stream routing.
            Port::Standard(source @ (1 | 2))
                if terminal.is_some() && self.terminal_stdio[usize::from(source)] =>
            {
                Ok(Inherited::Stream(
                    terminal
                        .ok_or(Error::Protocol)?
                        .duplicate(hyper_os::stream::CONNECTOR_RIGHTS)?,
                ))
            }
            // Unredirected output keeps its original transport. Relaying stdout
            // and stderr independently at every nested shell lets a later
            // stdout prompt overtake already-written stderr fragments.
            Port::Standard(1) => self
                .output
                .duplicate(StandardStream::Output)
                .map_err(Into::into),
            Port::Standard(2) => self
                .error
                .duplicate(StandardStream::Error)
                .map_err(Into::into),
            Port::Closed => {
                let (child, parent) = channel::create_pair()?;
                drop(parent);
                Ok(Inherited::Pipe(child))
            }
            port if fd == 0 => {
                let input = crate::input::Input::new(index, port)?;
                let child = input.connector()?;
                adapters.inputs.push(input);
                Ok(Inherited::Stream(child))
            }
            port => {
                let (child, parent) = channel::create_pair()?;
                adapters.routes.push(route::Route::new(
                    route::Endpoint::Owned(parent),
                    route::Endpoint::Data(port),
                    Some(index),
                    None,
                ));
                Ok(Inherited::Pipe(child))
            }
        }
    }
}

struct Running(Vec<OwnedHandle<ProcessObject>>);
impl Drop for Running {
    fn drop(&mut self) {
        for process in &self.0 {
            let _ = process.as_process_supervisor().request_stop();
        }
    }
}

fn relay_pipeline(
    processes: &[OwnedHandle<ProcessObject>],
    routes: &mut [route::Route],
    descriptors: &mut [crate::descriptor::Server],
    inputs: &mut [crate::input::Input],
    backend: &mut NativeBackend,
    controls: &mut [control::Supervisor],
    requests: &mut [job::Requests],
    mut job_events: Option<&mut hyper_terminal::JobEvents>,
    broadcaster: &mut job::Broadcaster,
    root_job: Option<usize>,
) -> Result<Vec<i32>, Error> {
    let mut terminated = vec![false; processes.len()];
    let mut first = 0;
    let mut terminated_by_request = vec![false; processes.len()];
    let mut termination_requested = vec![false; processes.len()];
    let mut exit_forwarded = false;
    loop {
        let mut progressed = backend.poll_control()?;
        if backend.exit_requested && !exit_forwarded {
            // Shell shutdown requests cleanup from its own children. This is
            // shell policy, not recursive semantics of a process signal.
            for control in controls.iter_mut() {
                control.request_terminate();
            }
            termination_requested.fill(true);
            exit_forwarded = true;
        }
        if let Some(events) = job_events.as_mut() {
            for _ in 0..8 {
                match events.try_receive() {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(hyper_os::Error::Status(hyper_os::Status::PEER_CLOSED)) => {
                        // Terminal failure closes both event paths. Disable
                        // this wait without unwinding Running and stopping
                        // children before their disconnected-I/O cleanup.
                        job_events = None;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
                match root_job {
                    Some(index) => backend.jobs[index].registry.request_terminate(),
                    None => broadcaster.request_terminate(),
                }
                progressed = true;
            }
        }
        progressed |= broadcaster.poll()?;
        // Fan-out precedes any default action. No Process supervisor authority
        // is transferred to the job registry, and no TaskGroup is stopped.
        for job in &mut backend.jobs {
            progressed |= job.poll()?;
        }
        for (index, ((control, request), process)) in controls
            .iter_mut()
            .zip(requests.iter_mut())
            .zip(processes)
            .enumerate()
        {
            if terminated[index] {
                continue;
            }
            if request.poll()? {
                control.request_terminate();
                termination_requested[index] = true;
                progressed = true;
            }
            match control.poll(0) {
                Ok(Some(control::Dispatch::Default)) => {
                    process.as_process_supervisor().request_stop()?;
                    terminated_by_request[index] = true;
                    progressed = true;
                }
                Ok(Some(control::Dispatch::Unavailable)) => {
                    eprintln!("sh: child termination handler unavailable");
                    progressed = true;
                }
                Ok(Some(_)) => progressed = true,
                Ok(None) => {}
                Err(error) => {
                    eprintln!("sh: child control: {error:?}");
                }
            }
        }
        for (index, process) in processes.iter().enumerate() {
            if !terminated[index] {
                terminated[index] = process.as_process_supervisor().info()?.terminal.is_some();
            }
        }
        for input in inputs.iter_mut() {
            if terminated[input.owner] {
                input.finish();
            } else {
                progressed |= input.poll()?;
            }
        }
        for descriptor in descriptors.iter_mut() {
            if terminated[descriptor.owner] {
                descriptor.finish();
            } else {
                progressed |= descriptor.poll()?;
            }
        }
        for offset in 0..routes.len() {
            let index = (first + offset) % routes.len();
            let route = &mut routes[index];
            if route.finished() {
                continue;
            }
            if route.consumer.is_some_and(|i| terminated[i]) {
                route.finish();
                continue;
            }
            let progress = route.poll()?;
            // Preserve finite output draining if a descendant retains stdout.
            if route.producer.is_some_and(|i| terminated[i]) && !progress && !route.has_pending() {
                route.finish();
            }
            progressed |= progress;
        }
        first = (first + 1) % routes.len().max(1);
        if terminated.iter().all(|done| *done)
            && routes.iter().all(route::Route::finished)
            && broadcaster.watch().is_none()
        {
            break;
        }
        if progressed {
            continue;
        }
        let mut waits = Vec::new();
        backend.control_waits(&mut waits);
        if let Some((handle, signals)) = broadcaster.watch() {
            waits.push(Source::Channel(handle, signals));
        }
        if let Some(events) = job_events.as_ref() {
            waits.push(Source::Channel(
                events.as_handle_ref(),
                hyper_os::wait::ObjectSignals::<ByteChannelObject>::READABLE
                    .union(hyper_os::wait::ObjectSignals::<ByteChannelObject>::PEER_CLOSED),
            ));
        }
        for (index, (control, request)) in controls.iter().zip(requests.iter()).enumerate() {
            if terminated[index] {
                continue;
            }
            if let Some(handle) = control.as_handle_ref() {
                waits.push(Source::Channel(handle, control.signals()));
            }
            if let Some((handle, signals)) = request.watch() {
                waits.push(Source::Channel(handle, signals));
            }
        }
        for input in inputs.iter() {
            input.waits(&mut waits);
        }
        for (index, process) in processes.iter().enumerate() {
            if !terminated[index] {
                waits.push(Source::Process(process.as_handle_ref()));
            }
        }
        for descriptor in descriptors.iter() {
            if let Some(wait) = descriptor.wait_item() {
                waits.push(wait);
            }
        }
        for route in routes.iter() {
            if let Some(wait) = route.wait_item() {
                waits.push(wait);
            }
        }
        reactor::wait(&waits)?;
    }
    processes
        .iter()
        .enumerate()
        .map(|(index, process)| {
            let info = process.as_process_supervisor().info()?;
            Ok(match info.terminal {
                Some(
                    ProcessTermination::ThreadExited { status }
                    | ProcessTermination::ProcessExited { status }
                    | ProcessTermination::LastThreadExited { status },
                ) => {
                    let status = (status & 255) as i32;
                    // Shell supervisors report cooperative interruption as 130.
                    // An ordinary exit 130 without a request does not interrupt
                    // the caller's command list; other exit codes are unchanged.
                    if status == 130 && termination_requested[index] {
                        backend.cancel_requested = true;
                    }
                    status
                }
                Some(ProcessTermination::Requested) if terminated_by_request[index] => {
                    // A foreground request is not itself shell cancellation:
                    // Ignore and cooperative cleanup retain their exit status.
                    // Only an applied default stop aborts the command list.
                    backend.cancel_requested = true;
                    130
                }
                _ => 125,
            })
        })
        .collect()
}
