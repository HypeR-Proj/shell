// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Executable selection and attenuation of the shell's command capabilities.

use super::{CommandAuthorities, Error, WORKING_DIRECTORY_RIGHTS};
use hyper_os::capability_channel::{CapabilityChannel, CapabilityDisposition};
use hyper_os::channel;
use hyper_os::handle::{
    ByteChannelObject, CapabilityChannelObject, OwnedHandle, Rights, RightsOffer,
};
use hyper_os::startup;
use hyper_os::task::ProcessBuilder;
use hyper_os::wait::{ObjectSignals, WaitItem, wait_many};
use hyper_os::{Error as OsError, Status};
use hyper_service::stdio::{Inherited, StandardStream};
use hyper_service::{filesystem, process, stdio, vm};
use hyper_shell::program::{Authority, Program};

pub(super) fn prepare_command(
    arguments: &[String],
    environment: &[String],
    authorities: &CommandAuthorities,
    child: ChildChannels,
) -> Result<ProcessBuilder, Error> {
    let name = arguments.first().ok_or(Error::InvalidCommand)?;
    let program =
        Program::resolve(name, &authorities.current_path).map_err(|_| Error::InvalidCommand)?;
    let executable = match authorities.root_directory.open(
        &program.path,
        hyper_service::process::EXECUTABLE_FILE_RIGHTS,
    ) {
        Ok(executable) => executable,
        Err(open_error) => {
            let reason = if matches!(open_error, OsError::Status(Status::NOT_FOUND)) {
                "command not found"
            } else {
                "cannot open command"
            };
            return Err(Error::Message(format!("{name}: {reason}: {open_error:?}")));
        }
    };
    // The session delegates one formal loader and library directory. The
    // dynamic loader also handles static ELF, so no boot archive is retained.
    let loader = &authorities.loader;
    let builder = ProcessBuilder::create(
        authorities.factory.as_handle_ref(),
        authorities.group.as_handle_ref(),
        authorities.domain.as_handle_ref(),
        executable.as_handle_ref(),
        loader.as_handle_ref(),
    )
    .map_err(Error::from)?;
    if program.is_shell() {
        process::ShellDelegation {
            environment: authorities.environment.as_handle_ref(),
            loader: authorities.loader.as_handle_ref(),
            libraries: authorities.library_directory.as_handle_ref(),
            tasks: authorities.task_inspector.as_handle_ref(),
            objects: authorities.object_inspector.as_handle_ref(),
            memory: authorities.memory_inspector.as_handle_ref(),
            cpus: authorities.cpu_inspector.as_handle_ref(),
            services: authorities
                .services
                .as_ref()
                .map(OwnedHandle::as_handle_ref),
            service_control: authorities
                .service_control
                .as_ref()
                .map(OwnedHandle::as_handle_ref),
            journal: authorities.journal.as_ref().map(OwnedHandle::as_handle_ref),
            filesystem: authorities
                .filesystem_control
                .as_ref()
                .map(OwnedHandle::as_handle_ref),
            virtual_machines: authorities
                .vm_connection
                .as_ref()
                .map(CapabilityChannel::as_handle_ref),
        }
        .add_to(&builder)?;
    } else {
        builder.add_handle_duplicate(
            loader.as_handle_ref(),
            process::CHILD_LOADER.as_raw(),
            RightsOffer::Exact(process::CHILD_LOADER_CONTRACT.allowed_rights()),
        )?;
    }
    builder.set_name(name).map_err(Error::from)?;
    for argument in arguments {
        builder.add_argument(argument)?;
    }
    for entry in environment {
        builder.add_environment(entry)?;
    }
    builder
        .add_handle_duplicate(
            authorities.library_directory.as_handle_ref(),
            startup::DYNAMIC_LIBRARY_DIRECTORY.as_raw(),
            RightsOffer::Exact(
                Rights::READ
                    .union(Rights::EXECUTE)
                    .union(Rights::PROTECT)
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
    child.input.add_to(&builder, StandardStream::Input)?;
    if let Some(terminal) = child.terminal {
        builder
            .add_handle_move(
                terminal.control,
                hyper_terminal::client::CONTROL.as_raw(),
                RightsOffer::Exact(hyper_terminal::client::CONTROL_RIGHTS),
            )
            .map_err(|failure| failure.error())?;
        builder
            .add_handle_move(
                terminal.events,
                hyper_terminal::client::EVENTS.as_raw(),
                RightsOffer::Exact(hyper_terminal::client::EVENT_RIGHTS),
            )
            .map_err(|failure| failure.error())?;
        builder
            .add_handle_move(
                terminal.stream,
                hyper_terminal::client::STREAM.as_raw(),
                RightsOffer::Exact(hyper_os::stream::CONNECTOR_RIGHTS),
            )
            .map_err(|failure| failure.error())?;
    }
    child.output.add_to(&builder, StandardStream::Output)?;
    match program.authority {
        Some(Authority::Services) => {
            for (handle, purpose) in [
                (&authorities.services, hyper_component::OBSERVE),
                (&authorities.service_control, hyper_component::CONTROL),
            ] {
                if let Some(handle) = handle {
                    builder.add_handle_duplicate(
                        handle.as_handle_ref(),
                        purpose.as_raw(),
                        RightsOffer::Exact(hyper_service_ipc::CLIENT_RIGHTS),
                    )?;
                }
            }
        }
        Some(Authority::Journal) => {
            if let Some(handle) = &authorities.journal {
                builder.add_handle_duplicate(
                    handle.as_handle_ref(),
                    hyper_journal::QUERY.as_raw(),
                    RightsOffer::Exact(hyper_service_ipc::CLIENT_RIGHTS),
                )?;
            }
        }
        Some(Authority::Filesystem) => {
            if let Some(control) = authorities.filesystem_control.as_ref() {
                builder
                    .add_handle_duplicate(
                        control.as_handle_ref(),
                        filesystem::CONTROL_CLIENT.as_raw(),
                        RightsOffer::Exact(Rights::WAIT.union(Rights::WRITE)),
                    )
                    .map_err(|_| Error::InvalidCommand)?;
            }
        }
        Some(Authority::Tasks) => builder
            .add_handle_duplicate(
                authorities.task_inspector.as_handle_ref(),
                startup::TASK_INSPECTOR.as_raw(),
                RightsOffer::Exact(Rights::INSPECT),
            )
            .map_err(|_| Error::InvalidCommand)?,
        Some(Authority::Handles) => {
            builder
                .add_handle_duplicate(
                    authorities.object_inspector.as_handle_ref(),
                    startup::OBJECT_INSPECTOR.as_raw(),
                    RightsOffer::Exact(Rights::INSPECT.union(Rights::INSPECT_DETAILS)),
                )
                .map_err(|_| Error::InvalidCommand)?;
            builder
                .add_handle_duplicate(
                    authorities.task_inspector.as_handle_ref(),
                    startup::TASK_INSPECTOR.as_raw(),
                    RightsOffer::Exact(Rights::INSPECT),
                )
                .map_err(|_| Error::InvalidCommand)?;
        }
        Some(Authority::Memory) => builder
            .add_handle_duplicate(
                authorities.memory_inspector.as_handle_ref(),
                startup::MEMORY_INSPECTOR.as_raw(),
                RightsOffer::Exact(Rights::INSPECT),
            )
            .map_err(|_| Error::InvalidCommand)?,
        Some(Authority::Profiling) => {
            if let Some(connector) = authorities.vm_connection.as_ref()
                && let Ok(observer) = connect_trace_observer(connector)
            {
                add_child_channel(
                    &builder,
                    observer,
                    vm::TRACE_OBSERVER.as_raw(),
                    Rights::WAIT.union(Rights::READ).union(Rights::WRITE),
                )?;
            }
            builder
                .add_handle_duplicate(
                    authorities.task_inspector.as_handle_ref(),
                    startup::TASK_INSPECTOR.as_raw(),
                    RightsOffer::Exact(Rights::INSPECT),
                )
                .map_err(|_| Error::InvalidCommand)?;
            builder
                .add_handle_duplicate(
                    authorities.cpu_inspector.as_handle_ref(),
                    startup::CPU_INSPECTOR.as_raw(),
                    RightsOffer::Exact(
                        Rights::INSPECT
                            .union(Rights::SET_ATTRIBUTES)
                            .union(Rights::INSPECT_DETAILS),
                    ),
                )
                .map_err(|_| Error::InvalidCommand)?;
        }
        Some(Authority::Activity) => {
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
        Some(Authority::VirtualMachines) if authorities.vm_connection.is_some() => {
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
    child.error.add_to(&builder, StandardStream::Error)?;
    Ok(builder)
}

fn connect_trace_observer(
    connector: &CapabilityChannel,
) -> hyper_os::Result<OwnedHandle<ByteChannelObject>> {
    let (client, server) = channel::create_pair()?;
    let mut server = Some(server);
    let deadline = hyper_os::time::deadline_after(std::time::Duration::from_millis(250))?.as_raw();
    hyper_service::io::send_capabilities(
        connector,
        vm::trace::CONNECT,
        &mut [CapabilityDisposition::move_handle(
            &mut server,
            RightsOffer::Exact(Rights::WAIT.union(Rights::READ).union(Rights::WRITE)),
        )?],
        deadline,
    )?;
    Ok(client)
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

pub(super) struct ChildChannels {
    pub input: Inherited,
    pub terminal: Option<hyper_terminal::PreparedClient>,
    pub output: Inherited,
    pub error: Inherited,
}
