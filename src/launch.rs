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
    pub read_settings: Option<hyper_terminal::Settings>,
}

impl Backend for NativeBackend {
    fn canonical_input(&mut self, enabled: bool) -> Result<(), String> {
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
        if let (Some(id), Some(terminal)) = (lease, self.authorities.terminal.as_mut()) {
            let cleanup = terminal.release_child(id);
            if result.is_ok() {
                cleanup.map_err(|e| format!("terminal release: {e:?}"))?;
            }
        }
        result.map_err(|e| e.to_string())
    }
}

impl NativeBackend {
    fn launch(&mut self, stages: Vec<Stage>, lease: &mut Option<u64>) -> Result<Vec<i32>, Error> {
        let mut adapters = Adapters::default();
        let mut descriptors = Vec::new();
        let mut builders = Vec::new();
        let mut pipes = Vec::new();
        for _ in 1..stages.len() {
            pipes.push(channel::create_pair()?);
        }
        let mut lease_owner = None;
        for (index, stage) in stages.into_iter().enumerate() {
            let client = if matches!(stage.streams.get(0), Port::Standard(0))
                && self.authorities.terminal.is_some()
                && lease.is_none()
            {
                let terminal = self.authorities.terminal.as_mut().ok_or(Error::Protocol)?;
                let client = terminal.prepare_child()?;
                *lease = Some(client.id);
                lease_owner = Some(index);
                Some(client)
            } else {
                None
            };
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
            let terminal = client.map(|c| (c.control, c.events));
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
                        vec!["/system/bin/sh".into(), "--worker".into()],
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
        for (index, builder) in builders.into_iter().enumerate() {
            running.0.push(builder.start().map_err(|e| e.error())?);
            if lease_owner == Some(index)
                && let (Some(id), Some(terminal)) = (*lease, self.authorities.terminal.as_mut())
            {
                let process = running.0.last().ok_or(Error::Protocol)?;
                terminal.bind_child(id, process.as_process_supervisor().observe()?)?;
            }
        }
        relay_pipeline(
            &running.0,
            &mut adapters.routes,
            &mut descriptors,
            &mut adapters.inputs,
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
                if let Some(terminal) = terminal {
                    Ok(Inherited::Stream(
                        terminal.duplicate(hyper_os::stream::CONNECTOR_RIGHTS)?,
                    ))
                } else {
                    Ok(self.input.duplicate(StandardStream::Input)?)
                }
            }
            Port::Standard(1 | 2) if terminal.is_some() => Ok(Inherited::Stream(
                terminal
                    .ok_or(Error::Protocol)?
                    .duplicate(hyper_os::stream::CONNECTOR_RIGHTS)?,
            )),
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
) -> Result<Vec<i32>, Error> {
    let mut terminated = vec![false; processes.len()];
    let mut first = 0;
    loop {
        for (index, process) in processes.iter().enumerate() {
            if !terminated[index] {
                terminated[index] = process.as_process_supervisor().info()?.terminal.is_some();
            }
        }
        let mut progressed = false;
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
        if terminated.iter().all(|done| *done) && routes.iter().all(route::Route::finished) {
            break;
        }
        if progressed {
            continue;
        }
        let mut waits = Vec::new();
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
        .map(|process| {
            let info = process.as_process_supervisor().info()?;
            Ok(match info.terminal {
                Some(
                    ProcessTermination::ThreadExited { status }
                    | ProcessTermination::ProcessExited { status }
                    | ProcessTermination::LastThreadExited { status },
                ) => (status & 255) as i32,
                _ => 125,
            })
        })
        .collect()
}
