// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Capability-preserving pipeline construction and foreground supervision.
use super::{CommandAuthorities, Error, WORKING_DIRECTORY_RIGHTS, write_terminal};
use crate::route;
use hyper_os::capability_channel::{CapabilityChannel, CapabilityDisposition};
use hyper_os::channel;
use hyper_os::fs::{File, FileRights};
use hyper_os::handle::{
    ByteChannelObject, CapabilityChannelObject, OwnedHandle, ProcessObject, Rights, RightsOffer,
};
use hyper_os::startup;
use hyper_os::task::{ProcessBuilder, ProcessInfo, ProcessTermination};
use hyper_os::wait::{ObjectSignals, WaitItem, wait_many};
use hyper_os::{Error as OsError, Status};
use hyper_service::{process, stdio, vm};
use hyper_shell::command::{CommandLine, Pipeline};

fn prepare_command(
    command: &CommandLine,
    source: &[u8],
    authorities: &CommandAuthorities,
    child: ChildChannels,
) -> Result<ProcessBuilder, Error> {
    let name = command.argument(0).ok_or(Error::InvalidCommand)?;
    let executable = match open_command(authorities, name) {
        Ok(executable) => executable,
        Err(open_error) => {
            let reason = if matches!(open_error, OsError::Status(Status::NOT_FOUND)) {
                "command not found"
            } else {
                "cannot open command"
            };
            write_terminal(
                format!(
                    "sh: {reason}: {name:?} (input: \"{}\"; {open_error:?})\n",
                    source.escape_ascii()
                )
                .as_bytes(),
            )?;
            return Err(Error::InvalidCommand);
        }
    };
    let builder = ProcessBuilder::create(
        authorities.factory.as_handle_ref(),
        authorities.group.as_handle_ref(),
        authorities.domain.as_handle_ref(),
        executable.as_handle_ref(),
    )
    .map_err(Error::from)?;
    builder.set_name(name).map_err(Error::from)?;
    for index in 0..command.len() {
        builder
            .add_argument(command.argument(index).ok_or(Error::InvalidCommand)?)
            .map_err(Error::from)?;
    }
    builder
        .add_handle_duplicate(
            authorities.library_directory.as_handle_ref(),
            startup::DYNAMIC_LIBRARY_DIRECTORY.as_raw(),
            RightsOffer::Exact(
                Rights::READ
                    .union(Rights::EXECUTE)
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER),
            ),
        )
        .map_err(|_| Error::InvalidCommand)?;
    builder
        .add_handle_duplicate(
            authorities.current_directory.as_handle_ref(),
            process::WORKING_DIRECTORY.as_raw(),
            RightsOffer::Exact(WORKING_DIRECTORY_RIGHTS.as_rights()),
        )
        .map_err(|_| Error::InvalidCommand)?;

    builder
        .add_handle_duplicate(
            authorities.root_directory.as_handle_ref(),
            startup::ROOT_DIRECTORY.as_raw(),
            RightsOffer::Exact(WORKING_DIRECTORY_RIGHTS.as_rights()),
        )
        .map_err(|_| Error::InvalidCommand)?;
    builder
        .add_handle_duplicate(
            authorities.factory.as_handle_ref(),
            startup::TASK_FACTORY.as_raw(),
            RightsOffer::Exact(
                Rights::CREATE_PROCESS
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER),
            ),
        )
        .map_err(|_| Error::InvalidCommand)?;
    builder
        .add_handle_duplicate(
            authorities.group.as_handle_ref(),
            startup::TASK_GROUP.as_raw(),
            RightsOffer::Exact(
                Rights::TASK_GROUP_ATTACH_PROCESS
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER),
            ),
        )
        .map_err(|_| Error::InvalidCommand)?;
    builder
        .add_handle_duplicate(
            authorities.domain.as_handle_ref(),
            startup::RESOURCE_DOMAIN.as_raw(),
            RightsOffer::Exact(
                Rights::RESOURCE_DOMAIN_SPONSOR
                    .union(Rights::DUPLICATE)
                    .union(Rights::TRANSFER),
            ),
        )
        .map_err(|_| Error::InvalidCommand)?;
    let ChildChannels {
        input: child_input,
        output: child_output,
        error: child_error,
    } = child;
    add_child_channel(
        &builder,
        child_input,
        stdio::STANDARD_INPUT.as_raw(),
        Rights::WAIT.union(Rights::READ),
    )?;
    add_child_channel(
        &builder,
        child_output,
        stdio::STANDARD_OUTPUT.as_raw(),
        Rights::WAIT.union(Rights::WRITE),
    )?;
    match name {
        "ps" | "/bin/ps" => builder
            .add_handle_duplicate(
                authorities.task_inspector.as_handle_ref(),
                startup::TASK_INSPECTOR.as_raw(),
                RightsOffer::Exact(Rights::INSPECT),
            )
            .map_err(|_| Error::InvalidCommand)?,
        "handle" | "/bin/handle" => builder
            .add_handle_duplicate(
                authorities.object_inspector.as_handle_ref(),
                startup::OBJECT_INSPECTOR.as_raw(),
                RightsOffer::Exact(Rights::INSPECT),
            )
            .map_err(|_| Error::InvalidCommand)?,
        "free" | "/bin/free" => builder
            .add_handle_duplicate(
                authorities.memory_inspector.as_handle_ref(),
                startup::MEMORY_INSPECTOR.as_raw(),
                RightsOffer::Exact(Rights::INSPECT),
            )
            .map_err(|_| Error::InvalidCommand)?,
        "top" | "/bin/top" => {
            builder
                .add_handle_duplicate(
                    authorities.task_inspector.as_handle_ref(),
                    startup::TASK_INSPECTOR.as_raw(),
                    RightsOffer::Exact(Rights::INSPECT),
                )
                .map_err(|_| Error::InvalidCommand)?;
            builder
                .add_handle_duplicate(
                    authorities.memory_inspector.as_handle_ref(),
                    startup::MEMORY_INSPECTOR.as_raw(),
                    RightsOffer::Exact(Rights::INSPECT),
                )
                .map_err(|_| Error::InvalidCommand)?;
            builder
                .add_handle_duplicate(
                    authorities.cpu_inspector.as_handle_ref(),
                    startup::CPU_INSPECTOR.as_raw(),
                    RightsOffer::Exact(Rights::INSPECT),
                )
                .map_err(|_| Error::InvalidCommand)?;
        }
        "vmm" | "/bin/vmm" if authorities.vm_connection.is_some() => {
            let (client_control, manager_control) = channel::create_pair().map_err(Error::from)?;
            let (manager_capabilities, client_capabilities) =
                CapabilityChannel::create().map_err(Error::from)?;
            connect_vm_manager(
                authorities
                    .vm_connection
                    .as_ref()
                    .ok_or(Error::InvalidCommand)?,
                manager_control,
                manager_capabilities,
            )?;
            add_child_channel(
                &builder,
                client_control,
                vm::CLIENT_CONTROL.as_raw(),
                vm::CLIENT_CONTROL_CONTRACT.required_rights(),
            )?;
            builder
                .add_handle_move(
                    client_capabilities.into_handle(),
                    vm::CLIENT_CAPABILITIES.as_raw(),
                    RightsOffer::Exact(vm::CLIENT_CAPABILITIES_CONTRACT.required_rights()),
                )
                .map_err(|failure| Error::from(failure.error()))?;
        }
        _ => {}
    }
    add_child_channel(
        &builder,
        child_error,
        stdio::STANDARD_ERROR.as_raw(),
        Rights::WAIT.union(Rights::WRITE),
    )?;
    builder.seal().map_err(Error::from)?;
    Ok(builder)
}

pub(super) fn launch_pipeline(
    pipeline: &Pipeline,
    source: &[u8],
    authorities: &CommandAuthorities,
    input: &OwnedHandle<ByteChannelObject>,
    output: &OwnedHandle<ByteChannelObject>,
    error: &OwnedHandle<ByteChannelObject>,
) -> Result<(), Error> {
    use route::{Endpoint, Route};
    // Reject state-changing builtins before opening/truncating any redirects.
    for stage in &pipeline.0 {
        if matches!(stage.command.argument(0), Some("cd" | "exit")) {
            write_terminal(b"sh: cd and exit require a standalone command without redirection\n")?;
            return Ok(());
        }
    }
    let mut routes = Vec::new();
    let mut builders = Vec::new();
    let (parent_input, next_input) = channel::create_pair()?;
    let mut next_input = Some(next_input);
    let mut terminal_input = Some(parent_input);
    for (index, stage) in pipeline.0.iter().enumerate() {
        let last = index + 1 == pipeline.0.len();
        let (mut child_output, following_input) = channel::create_pair()?;
        let mut child_input = next_input.take().ok_or(Error::Protocol)?;
        let (child_error, parent_error) = channel::create_pair()?;
        let mut input_file = None;
        let mut output_file = None;
        let mut error_file = None;
        // Left-to-right opening preserves truncation side effects of earlier
        // redirects. Only the final destination for each stream is retained.
        for redirect in &stage.redirects {
            let path = authorities
                .current_path
                .resolve(&redirect.path)
                .map_err(|_| Error::InvalidCommand)?;
            let path = path.as_str().map_err(|_| Error::InvalidCommand)?;
            let file = if redirect.stream == 0 {
                std::fs::File::open(path)
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .append(redirect.append)
                    .truncate(!redirect.append)
                    .open(path)
            }
            .map_err(Error::Io)?;
            match redirect.stream {
                0 => input_file = Some(file),
                1 => output_file = Some(file),
                _ => error_file = Some(file),
            }
        }
        if let Some(file) = input_file {
            let (parent, child) = channel::create_pair()?;
            child_input = child;
            if index == 0 {
                terminal_input = None;
            }
            routes.push(Route::new(
                Endpoint::File(file),
                Endpoint::Owned(parent),
                None,
                Some(index),
            ));
        } else if index == 0 {
            routes.push(Route::new(
                Endpoint::Terminal(input),
                Endpoint::Owned(terminal_input.take().ok_or(Error::Protocol)?),
                None,
                Some(index),
            ));
        }
        if let Some(file) = output_file {
            let (child, parent) = channel::create_pair()?;
            child_output = child;
            routes.push(Route::new(
                Endpoint::Owned(parent),
                Endpoint::File(file),
                Some(index),
                None,
            ));
        }
        let error_destination = error_file.map_or(Endpoint::Terminal(error), Endpoint::File);
        routes.push(Route::new(
            Endpoint::Owned(parent_error),
            error_destination,
            Some(index),
            None,
        ));
        let builtin = matches!(stage.command.argument(0), Some("pwd" | "clear" | "help"));
        let worker;
        let command = if builtin {
            let mut words = vec!["/bin/sh".to_owned(), "--builtin".to_owned()];
            words.extend(stage.command.arguments().map(str::to_owned));
            worker = CommandLine::parse(
                shlex::try_join(words.iter().map(String::as_str))
                    .map_err(|_| Error::InvalidCommand)?
                    .as_bytes(),
            )
            .map_err(|_| Error::InvalidCommand)?;
            &worker
        } else {
            &stage.command
        };
        builders.push(prepare_command(
            command,
            source,
            authorities,
            ChildChannels {
                input: child_input,
                output: child_output,
                error: child_error,
            },
        )?);
        next_input = Some(following_input);
        if last {
            routes.push(Route::new(
                Endpoint::Owned(next_input.take().ok_or(Error::Protocol)?),
                Endpoint::Terminal(output),
                Some(index),
                None,
            ));
        }
    }
    drop(next_input);
    let mut running = Running(Vec::new());
    for builder in builders {
        running.0.push(
            builder
                .start()
                .map_err(|failure| Error::from(failure.error()))?,
        );
    }
    relay_pipeline(&running.0, &mut routes)?;
    Ok(())
}

struct Running(Vec<OwnedHandle<ProcessObject>>);
impl Drop for Running {
    fn drop(&mut self) {
        // Also covers a later start or relay failure after earlier stages ran.
        for process in &self.0 {
            let _ = process.as_process_supervisor().request_stop();
        }
    }
}

fn connect_vm_manager(
    connector: &CapabilityChannel,
    control: OwnedHandle<ByteChannelObject>,
    capabilities: CapabilityChannel,
) -> Result<(), Error> {
    let mut control = Some(control);
    let mut capabilities = Some(capabilities.into_handle());
    loop {
        let waits = [WaitItem::new(
            connector.as_handle_ref(),
            ObjectSignals::<CapabilityChannelObject>::PEER_RECEIVING
                .union(ObjectSignals::<CapabilityChannelObject>::PEER_CLOSED),
        )];
        let observation = wait_many(&waits, hyper_os::DEADLINE_INFINITE).map_err(Error::from)?;
        if !ObjectSignals::<CapabilityChannelObject>::PEER_RECEIVING
            .is_present_in(observation.observed)
        {
            return Err(Error::from(OsError::Status(Status::PEER_CLOSED)));
        }
        let control_disposition = CapabilityDisposition::move_handle(
            &mut control,
            RightsOffer::Exact(Rights::WAIT.union(Rights::READ).union(Rights::WRITE)),
        )
        .map_err(Error::from)?;
        let capabilities_disposition = CapabilityDisposition::move_handle(
            &mut capabilities,
            RightsOffer::Exact(Rights::WAIT.union(Rights::WRITE)),
        )
        .map_err(Error::from)?;
        match connector.try_send(
            &vm::ManagerConnectionRequest.encode(),
            &mut [control_disposition, capabilities_disposition],
        ) {
            Ok(()) => return Ok(()),
            Err(OsError::Status(Status::WOULD_BLOCK)) => {}
            Err(error) => return Err(Error::from(error)),
        }
    }
}

fn add_child_channel(
    builder: &ProcessBuilder,
    channel: OwnedHandle<ByteChannelObject>,
    purpose: u32,
    rights: Rights,
) -> Result<(), Error> {
    let rights = if [
        stdio::STANDARD_INPUT.as_raw(),
        stdio::STANDARD_OUTPUT.as_raw(),
        stdio::STANDARD_ERROR.as_raw(),
    ]
    .contains(&purpose)
    {
        rights.union(Rights::DUPLICATE).union(Rights::TRANSFER)
    } else {
        rights
    };
    builder
        .add_handle_move(channel, purpose, RightsOffer::Exact(rights))
        .map_err(|failure| Error::from(failure.error()))
}

fn relay_pipeline(
    processes: &[OwnedHandle<ProcessObject>],
    routes: &mut [route::Route<'_>],
) -> Result<(), Error> {
    let mut terminated = vec![false; processes.len()];
    let mut first = 0;
    loop {
        for (index, process) in processes.iter().enumerate() {
            if !terminated[index] {
                terminated[index] = process.as_process_supervisor().info()?.terminal.is_some();
            }
        }
        let mut progressed = false;
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
        for (index, process) in processes.iter().enumerate() {
            if !terminated[index] {
                waits.push(WaitItem::new(
                    process.as_handle_ref(),
                    ObjectSignals::<ProcessObject>::TERMINATED,
                ));
            }
        }
        for route in routes.iter() {
            if let Some(wait) = route.wait_item() {
                waits.push(wait);
            }
        }
        wait_many(&waits, hyper_os::DEADLINE_INFINITE)?;
    }
    if let Some(last) = processes.last()
        && !process_succeeded(last.as_process_supervisor().info()?)
    {
        write_terminal(b"sh: command failed\n")?;
    }
    Ok(())
}

fn process_succeeded(info: ProcessInfo) -> bool {
    matches!(
        info.terminal,
        Some(
            ProcessTermination::ThreadExited { status: 0 }
                | ProcessTermination::ProcessExited { status: 0 }
                | ProcessTermination::LastThreadExited { status: 0 }
        )
    )
}

fn open_command(authorities: &CommandAuthorities, name: &str) -> Result<File, OsError> {
    if name.starts_with('/') {
        return authorities.root_directory.open(name, FileRights::EXECUTE);
    }
    if name.contains('/') {
        let path = authorities
            .current_path
            .resolve(name)
            .map_err(|_| OsError::InvalidPath)?;
        return authorities.root_directory.open(
            path.as_str().map_err(|_| OsError::InvalidPath)?,
            FileRights::EXECUTE,
        );
    }
    authorities
        .root_directory
        .open(&format!("/bin/{name}"), FileRights::EXECUTE)
}

struct ChildChannels {
    input: OwnedHandle<ByteChannelObject>,
    output: OwnedHandle<ByteChannelObject>,
    error: OwnedHandle<ByteChannelObject>,
}
