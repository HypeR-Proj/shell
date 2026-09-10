// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Initial Native command shell and capability-scoped command launcher.

use clap::Parser;
use hyper_os::capability_channel::{CapabilityChannel, CapabilityDisposition};
use hyper_os::channel;
use hyper_os::fs::{Directory, DirectoryRights, File, FileRights};
use hyper_os::handle::{
    ByteChannelObject, CapabilityChannelObject, CpuInspectorObject, MemoryInspectorObject,
    ObjectInspectorObject, OwnedHandle, ProcessObject, ResourceDomainObject, Rights, RightsOffer,
    TaskFactoryObject, TaskGroupObject, TaskInspectorObject,
};
use hyper_os::startup::{self, Startup};
use hyper_os::task::{ProcessBuilder, ProcessInfo, ProcessTermination};
use hyper_os::wait::{ObjectSignals, WaitItem, wait_many};
use hyper_os::{Error as OsError, Status};
use hyper_service::{process, stdio, vm};
use hyper_shell::cli::{Builtin, BuiltinCommand};
use hyper_shell::command::{CommandLine, MAX_LINE_BYTES};
use hyper_shell::path::CanonicalPath;
use std::io::Write;
use std::process::ExitCode;

const INPUT_CHUNK_BYTES: usize = 256;
const READY_MESSAGE: &[u8] = b"HypeR session: console ready\n";
const PROMPT: &[u8] = b"hyper-sh$ ";
const WORKING_DIRECTORY_RIGHTS: DirectoryRights = DirectoryRights::READ
    .union(DirectoryRights::WRITE)
    .union(DirectoryRights::INSPECT)
    .union(DirectoryRights::EXECUTE)
    .union(DirectoryRights::DUPLICATE)
    .union(DirectoryRights::TRANSFER);

fn application_main(mut startup: Startup<'_>) -> ExitCode {
    match run(&mut startup) {
        Ok(code) => code,
        Err(_) => ExitCode::FAILURE,
    }
}

fn run(startup: &mut Startup<'_>) -> Result<ExitCode, Error> {
    hyper_os::require_core_abi().map_err(Error::from)?;
    let input = hyper_rt::process::stdin().map_err(Error::from)?;
    let output = hyper_rt::process::stdout().map_err(Error::from)?;
    let error = hyper_rt::process::stderr().map_err(Error::from)?;
    let root_directory = startup.take_root_directory().map_err(Error::from)?;
    let current_directory = root_directory
        .open_directory("/", WORKING_DIRECTORY_RIGHTS)
        .map_err(Error::from)?;
    let library_directory = Directory::from_handle(
        startup
            .take(process::CHILD_LIBRARY_DIRECTORY)
            .map_err(Error::from)?,
    );
    let factory = startup.take(startup::TASK_FACTORY).map_err(Error::from)?;
    let group = startup.take(startup::TASK_GROUP).map_err(Error::from)?;
    let domain = startup
        .take(startup::RESOURCE_DOMAIN)
        .map_err(Error::from)?;
    let task_inspector = startup.take(startup::TASK_INSPECTOR).map_err(Error::from)?;
    let object_inspector = startup
        .take(startup::OBJECT_INSPECTOR)
        .map_err(Error::from)?;
    let memory_inspector = startup
        .take(startup::MEMORY_INSPECTOR)
        .map_err(Error::from)?;
    let cpu_inspector = startup.take(startup::CPU_INSPECTOR).map_err(Error::from)?;
    let vm_connection =
        CapabilityChannel::from_handle(startup.take(vm::MANAGER_CONNECTION).map_err(Error::from)?);
    let mut authorities = CommandAuthorities {
        root_directory,
        current_directory,
        current_path: CanonicalPath::root(),
        library_directory,
        factory,
        group,
        domain,
        task_inspector,
        object_inspector,
        memory_inspector,
        cpu_inspector,
        vm_connection,
    };

    write(output, READY_MESSAGE)?;
    write(output, PROMPT)?;

    let mut line = Vec::with_capacity(MAX_LINE_BYTES);
    let mut discard_line = false;
    let mut previous_was_carriage_return = false;
    let mut input_chunk = [0_u8; INPUT_CHUNK_BYTES];
    loop {
        let count = input
            .as_byte_channel()
            .receive(&mut input_chunk)
            .map_err(Error::from)?;
        let bytes = input_chunk.get(..count).ok_or(Error::Protocol)?;
        for byte in bytes.iter().copied() {
            if byte == b'\n' && previous_was_carriage_return {
                previous_was_carriage_return = false;
                continue;
            }
            previous_was_carriage_return = byte == b'\r';
            match byte {
                b'\r' | b'\n' => {
                    write(output, b"\r\n")?;
                    if discard_line {
                        write(error, &[PROMPT, b"sh: command line is too long\n"].concat())?;
                    } else if !line.is_empty() {
                        let command = line.as_slice();
                        match execute_line(command, &mut authorities, input, output, error) {
                            Ok(CommandFlow::Continue) => {}
                            Ok(CommandFlow::Exit) => return Ok(ExitCode::SUCCESS),
                            Err(command_error) => {
                                writeln!(std::io::stderr(), "sh: {command_error}")
                                    .map_err(Error::Io)?
                            }
                        }
                    }
                    line.clear();
                    discard_line = false;
                    write(output, PROMPT)?;
                }
                0x03 => {
                    line.clear();
                    discard_line = false;
                    write(output, b"^C\r\n")?;
                    write(output, PROMPT)?;
                }
                0x04 if line.is_empty() => return Ok(ExitCode::SUCCESS),
                0x08 | 0x7f => {
                    // DEL must remain an editing key even on an empty line;
                    // otherwise it falls through into the printable bytes.
                    if !discard_line && line.pop().is_some() {
                        write(output, b"\x08 \x08")?;
                    }
                }
                byte if byte == b'\t' || byte >= 0x20 => {
                    if discard_line {
                        continue;
                    }
                    if line.len() == MAX_LINE_BYTES {
                        discard_line = true;
                        continue;
                    }
                    line.push(byte);
                    write(output, std::slice::from_ref(&byte))?;
                }
                _ => {}
            }
        }
    }
}

fn execute_line(
    bytes: &[u8],
    authorities: &mut CommandAuthorities,
    input: &OwnedHandle<ByteChannelObject>,
    output: &OwnedHandle<ByteChannelObject>,
    error: &OwnedHandle<ByteChannelObject>,
) -> Result<CommandFlow, Error> {
    let command = match CommandLine::parse(bytes) {
        Ok(command) => command,
        Err(_) => {
            write(error, b"sh: invalid command syntax\n")?;
            return Ok(CommandFlow::Continue);
        }
    };
    if command.is_empty() {
        return Ok(CommandFlow::Continue);
    }
    let Some(name) = command.argument(0) else {
        write(error, b"sh: command is not valid UTF-8\n")?;
        return Ok(CommandFlow::Continue);
    };
    if !matches!(name, "cd" | "pwd" | "echo" | "clear" | "exit" | "help") {
        launch_command(&command, bytes, authorities, input, output, error)?;
        return Ok(CommandFlow::Continue);
    }
    let builtin = match Builtin::try_parse_from(std::iter::once("sh").chain(command.arguments())) {
        Ok(args) => args.command,
        Err(error) => {
            error.print().map_err(Error::Io)?;
            return Ok(CommandFlow::Continue);
        }
    };
    match builtin {
        BuiltinCommand::Help => {
            writeln!(std::io::stdout(), "builtins: cd clear echo exit help pwd")
                .map_err(Error::Io)?
        }
        BuiltinCommand::Cd(args) => builtin_cd(&args.directory, authorities, error)?,
        BuiltinCommand::Pwd => writeln!(
            std::io::stdout(),
            "{}",
            authorities
                .current_path
                .as_str()
                .map_err(|_| Error::InvalidCommand)?
        )
        .map_err(Error::Io)?,
        BuiltinCommand::Echo(args) => {
            writeln!(std::io::stdout(), "{}", args.words.join(" ")).map_err(Error::Io)?
        }
        BuiltinCommand::Clear => std::io::stdout()
            .write_all(b"\x1b[2J\x1b[H")
            .map_err(Error::Io)?,
        BuiltinCommand::Exit => return Ok(CommandFlow::Exit),
    }
    Ok(CommandFlow::Continue)
}

fn builtin_cd(
    target: &str,
    authorities: &mut CommandAuthorities,
    error: &OwnedHandle<ByteChannelObject>,
) -> Result<(), Error> {
    let path = match authorities.current_path.resolve(target) {
        Ok(path) => path,
        Err(_) => {
            write(error, b"cd: invalid directory path\n")?;
            return Ok(());
        }
    };
    let path_text = path.as_str().map_err(|_| Error::InvalidCommand)?;
    let directory = match authorities
        .root_directory
        .open_directory(path_text, WORKING_DIRECTORY_RIGHTS)
    {
        Ok(directory) => directory,
        Err(_) => {
            write(error, b"cd: cannot open directory\n")?;
            return Ok(());
        }
    };
    authorities.current_directory = directory;
    authorities.current_path = path;
    Ok(())
}

fn launch_command(
    command: &CommandLine,
    source: &[u8],
    authorities: &CommandAuthorities,
    input: &OwnedHandle<ByteChannelObject>,
    output: &OwnedHandle<ByteChannelObject>,
    error: &OwnedHandle<ByteChannelObject>,
) -> Result<(), Error> {
    let name = command.argument(0).ok_or(Error::InvalidCommand)?;
    let executable = match open_command(authorities, name) {
        Ok(executable) => executable,
        Err(open_error) => {
            let reason = if matches!(open_error, OsError::Status(Status::NOT_FOUND)) {
                "command not found"
            } else {
                "cannot open command"
            };
            write(
                error,
                format!(
                    "sh: {reason}: {name:?} (input: \"{}\"; {open_error:?})\n",
                    source.escape_ascii()
                )
                .as_bytes(),
            )?;
            return Ok(());
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
    let (parent_input, child_input) = channel::create_pair().map_err(Error::from)?;
    let (child_output, parent_output) = channel::create_pair().map_err(Error::from)?;
    let (child_error, parent_error) = channel::create_pair().map_err(Error::from)?;
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
        "vmm" | "/bin/vmm" => {
            let (client_control, manager_control) = channel::create_pair().map_err(Error::from)?;
            let (manager_capabilities, client_capabilities) =
                CapabilityChannel::create().map_err(Error::from)?;
            connect_vm_manager(
                &authorities.vm_connection,
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
    let process = builder
        .start()
        .map_err(|failure| Error::from(failure.error()))?;
    supervise_command(
        process,
        ChildChannels {
            input: parent_input,
            output: parent_output,
            error: parent_error,
        },
        ShellChannels {
            input,
            output,
            error,
        },
    )
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

fn supervise_command(
    process: OwnedHandle<ProcessObject>,
    child: ChildChannels,
    shell: ShellChannels<'_>,
) -> Result<(), Error> {
    let readable = ObjectSignals::<ByteChannelObject>::READABLE;
    let shell_readable = readable.union(ObjectSignals::<ByteChannelObject>::PEER_CLOSED);
    let terminated = ObjectSignals::<ProcessObject>::TERMINATED;
    let waits = [
        WaitItem::new(child.output.as_handle_ref(), readable),
        WaitItem::new(child.error.as_handle_ref(), readable),
        WaitItem::new(shell.input.as_handle_ref(), shell_readable),
        WaitItem::new(process.as_handle_ref(), terminated),
    ];
    let mut buffer = [0_u8; channel::MAX_MESSAGE_BYTES];
    loop {
        let observation = wait_many(&waits, hyper_os::DEADLINE_INFINITE).map_err(Error::from)?;
        match observation.index {
            0 => route_message(&child.output, shell.output, &mut buffer)?,
            1 => route_message(&child.error, shell.error, &mut buffer)?,
            2 if ObjectSignals::<ByteChannelObject>::READABLE
                .is_present_in(observation.observed) =>
            {
                route_message(shell.input, &child.input, &mut buffer)?;
            }
            2 => {
                let _ = process.as_process_supervisor().request_stop();
                return Err(Error::InputClosed);
            }
            3 => break,
            _ => return Err(Error::Protocol),
        }
    }
    drain_channel(&child.output, shell.output, &mut buffer)?;
    drain_channel(&child.error, shell.error, &mut buffer)?;
    let info = process
        .as_process_supervisor()
        .info()
        .map_err(Error::from)?;
    if !process_succeeded(info) {
        write(shell.error, b"sh: command failed\n")?;
    }
    Ok(())
}

fn route_message(
    source: &OwnedHandle<ByteChannelObject>,
    destination: &OwnedHandle<ByteChannelObject>,
    buffer: &mut [u8],
) -> Result<(), Error> {
    let count = source
        .as_byte_channel()
        .receive(buffer)
        .map_err(Error::from)?;
    write(destination, buffer.get(..count).ok_or(Error::Protocol)?)
}

fn drain_channel(
    source: &OwnedHandle<ByteChannelObject>,
    destination: &OwnedHandle<ByteChannelObject>,
    buffer: &mut [u8],
) -> Result<(), Error> {
    loop {
        match source.as_byte_channel().try_receive(buffer) {
            Ok(count) => write(destination, buffer.get(..count).ok_or(Error::Protocol)?)?,
            Err(OsError::Status(status))
                if status == Status::WOULD_BLOCK || status == Status::PEER_CLOSED =>
            {
                return Ok(());
            }
            Err(error) => return Err(Error::from(error)),
        }
    }
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

fn write(destination: &OwnedHandle<ByteChannelObject>, bytes: &[u8]) -> Result<(), Error> {
    destination
        .as_byte_channel()
        .send(bytes)
        .map_err(Error::from)
}

struct CommandAuthorities {
    root_directory: Directory,
    current_directory: Directory,
    current_path: CanonicalPath,
    library_directory: Directory,
    factory: OwnedHandle<TaskFactoryObject>,
    group: OwnedHandle<TaskGroupObject>,
    domain: OwnedHandle<ResourceDomainObject>,
    task_inspector: OwnedHandle<TaskInspectorObject>,
    object_inspector: OwnedHandle<ObjectInspectorObject>,
    memory_inspector: OwnedHandle<MemoryInspectorObject>,
    cpu_inspector: OwnedHandle<CpuInspectorObject>,
    vm_connection: CapabilityChannel,
}

struct ChildChannels {
    input: OwnedHandle<ByteChannelObject>,
    output: OwnedHandle<ByteChannelObject>,
    error: OwnedHandle<ByteChannelObject>,
}

struct ShellChannels<'a> {
    input: &'a OwnedHandle<ByteChannelObject>,
    output: &'a OwnedHandle<ByteChannelObject>,
    error: &'a OwnedHandle<ByteChannelObject>,
}

enum CommandFlow {
    Continue,
    Exit,
}

#[derive(Debug)]
enum Error {
    OperatingSystem(OsError),
    Io(std::io::Error),
    InvalidCommand,
    InputClosed,
    Protocol,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OperatingSystem(error) => write!(formatter, "Native operation failed: {error:?}"),
            Self::Io(error) => write!(formatter, "I/O failed: {error}"),
            Self::InvalidCommand => formatter.write_str("command launch failed"),
            Self::InputClosed => formatter.write_str("input channel closed"),
            Self::Protocol => formatter.write_str("protocol violation"),
        }
    }
}

impl From<OsError> for Error {
    fn from(error: OsError) -> Self {
        Self::OperatingSystem(error)
    }
}

fn main() -> ExitCode {
    hyper_shell::cli::Shell::parse();
    match hyper_rt::process::startup() {
        Ok(startup) => application_main(startup),
        Err(_) => ExitCode::FAILURE,
    }
}
