// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Formal system shell. Bootstrap uses its separate byte-stream fallback shell.

// Select the shared parser implementation for the formal environment.
#[cfg(target_os = "hyper")]
extern crate hyper_clap_shared as _;

mod capabilities;
mod descriptor;
mod input;
mod launch;
mod reactor;
mod route;
use hyper_shell::interpreter::{Backend, Flow, Shell};

use clap::Parser;
use hyper_os::Error as OsError;
use hyper_os::capability_channel::CapabilityChannel;
use hyper_os::fs::{Directory, DirectoryRights, File};
use hyper_os::handle::{
    CapabilityChannelObject, CpuInspectorObject, MemoryInspectorObject, ObjectInspectorObject,
    OwnedHandle, ResourceDomainObject, TaskFactoryObject, TaskGroupObject, TaskInspectorObject,
};
use hyper_os::startup::{self, Startup};
use hyper_service::stdio::{Inherited, StandardStream};
use hyper_service::{filesystem, process, vm};
use hyper_shell::editor::{Action, Editor};
use hyper_shell::interactive;
use hyper_shell::path::CanonicalPath;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

const INPUT_CHUNK_BYTES: usize = 1;
const READY_MESSAGE: &[u8] = b"HypeR session: console ready\n";
const PROMPT: &[u8] = b"hyper-sh$ ";
const WORKING_DIRECTORY_RIGHTS: DirectoryRights = DirectoryRights::READ
    .union(DirectoryRights::WRITE)
    .union(DirectoryRights::SET_ATTRIBUTES)
    .union(DirectoryRights::LOCK_FILE)
    .union(DirectoryRights::INSPECT)
    .union(DirectoryRights::EXECUTE)
    .union(DirectoryRights::PROTECT)
    .union(DirectoryRights::DUPLICATE)
    .union(DirectoryRights::TRANSFER);

fn run(startup: &mut Startup<'_>, args: &hyper_shell::cli::Shell) -> Result<ExitCode, Error> {
    hyper_os::require_core_abi().map_err(Error::from)?;
    let input = inherited_stream(StandardStream::Input)?;
    let output = inherited_stream(StandardStream::Output)?;
    let error = inherited_stream(StandardStream::Error)?;
    let terminal = hyper_terminal::Terminal::take(startup)?;
    let root_directory = startup.take_root_directory().map_err(Error::from)?;
    let environment = Directory::from_handle(
        startup
            .take(process::ENVIRONMENT_DIRECTORY)
            .map_err(Error::from)?,
    );
    // Keep the initial cwd attached to the namespace root. Opening "/" would
    // instead retain the old ramdisk directory across the init root switch.
    let current_directory = root_directory
        .scope_at(&root_directory, WORKING_DIRECTORY_RIGHTS)
        .map_err(Error::from)?;
    let library_directory = Directory::from_handle(
        startup
            .take(process::CHILD_LIBRARY_DIRECTORY)
            .map_err(Error::from)?,
    );
    let loader = File::from_handle(startup.take(process::CHILD_LOADER).map_err(Error::from)?);
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
    let vm_connection = startup
        .take_optional(vm::MANAGER_CONNECTION)
        .map_err(Error::from)?
        .map(CapabilityChannel::from_handle);
    let filesystem_control = startup
        .take_optional(filesystem::CONTROL_CLIENT)
        .map_err(Error::from)?;
    let services = startup.take_optional(hyper_component::OBSERVE)?;
    let service_control = startup.take_optional(hyper_component::CONTROL)?;
    let journal = startup.take_optional(hyper_journal::QUERY)?;
    let authorities = CommandAuthorities {
        services,
        service_control,
        journal,
        root_directory,
        environment,
        current_directory,
        current_path: CanonicalPath::root(),
        library_directory,
        loader,
        factory,
        group,
        domain,
        task_inspector,
        object_inspector,
        memory_inspector,
        cpu_inspector,
        vm_connection,
        filesystem_control,
        terminal,
    };

    let mut state = hyper_shell::interpreter::State::default();
    for (name, value) in std::env::vars() {
        if hyper_shell::syntax::identifier(&name) {
            state.variables.insert(
                name,
                hyper_shell::interpreter::Variable {
                    value,
                    exported: true,
                    readonly: false,
                },
            );
        }
    }
    state.cwd = std::env::current_dir()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned();
    let mut shell = Shell::new(
        state,
        launch::NativeBackend {
            authorities,
            input,
            output,
            error,
            terminal_stdio: [
                std::io::stdin().is_terminal(),
                std::io::stdout().is_terminal(),
                std::io::stderr().is_terminal(),
            ],
            read_settings: None,
        },
    );
    shell
        .backend
        .directory(&shell.state.cwd)
        .map_err(Error::Message)?;
    if args.worker {
        let channel = startup.take(launch::WORKER_STATE)?;
        let mut bytes = Vec::new();
        let mut chunk = [0; hyper_os::channel::MAX_MESSAGE_BYTES];
        loop {
            match channel.as_byte_channel().receive(&mut chunk) {
                Ok(n) => {
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.len() > hyper_shell::syntax::MAX_SCRIPT_BYTES {
                        return Err(Error::Protocol);
                    }
                }
                Err(OsError::Status(hyper_os::Status::PEER_CLOSED)) => break,
                Err(error) => return Err(error.into()),
            }
        }
        let worker: hyper_shell::interpreter::Worker =
            serde_json::from_slice(&bytes).map_err(|e| Error::Message(e.to_string()))?;
        for (fd, source) in &worker.aliases {
            if !(3..64).contains(fd) || *source > 2 {
                return Err(Error::Protocol);
            }
            shell.streams.set(*fd, shell.streams.get(*source));
        }
        if !worker.descriptors.is_empty() {
            let connector = std::rc::Rc::new(startup.take(descriptor::CONNECTOR)?);
            for &fd in &worker.descriptors {
                if !(3..64).contains(&fd) {
                    return Err(Error::Protocol);
                }
                shell.streams.set(
                    fd,
                    hyper_shell::interpreter::io::Port::Remote(std::rc::Rc::new(
                        descriptor::Remote {
                            channel: connector.clone(),
                            fd,
                        },
                    )),
                );
            }
        }
        let status = shell.worker(worker).map_err(Error::Message)?;
        return Ok(exit_code(match shell.flow {
            Flow::Exit(code) => code,
            _ => status,
        }));
    }
    let interactive = args.interactive
        || args.command.is_none()
            && args.arguments.is_empty()
            && shell.backend.terminal_stdio[0]
            && shell.backend.authorities.terminal.is_some();
    if let Some(command) = &args.command {
        if let Some(name) = args.arguments.first() {
            shell.state.name = name.clone();
            shell.state.positional = args.arguments[1..].to_vec();
        }
        return run_script(&mut shell, command, args.check);
    }
    if let Some(path) = args.arguments.first() {
        shell.state.name = path.clone();
        shell.state.positional = args.arguments[1..].to_vec();
        let source = read_script(&shell.state.path(path))?;
        return run_script(&mut shell, &source, args.check);
    }
    if !interactive {
        let mut source = String::new();
        std::io::stdin()
            .take(hyper_shell::syntax::MAX_SCRIPT_BYTES as u64 + 1)
            .read_to_string(&mut source)
            .map_err(Error::Io)?;
        return run_script(&mut shell, &source, args.check);
    }
    if let Some(terminal) = shell.backend.authorities.terminal.as_mut() {
        let mut settings = hyper_terminal::Settings::raw();
        settings.output.lf_to_crlf = true;
        terminal.set_settings(settings)?;
    }
    interactive_shell(&mut shell)
}

fn inherited_stream(which: StandardStream) -> Result<Inherited, Error> {
    let connector = match which {
        StandardStream::Input => hyper_rt::process::stdin_stream()?,
        StandardStream::Output => hyper_rt::process::stdout_stream()?,
        StandardStream::Error => hyper_rt::process::stderr_stream()?,
    };
    if let Some(connector) = connector {
        return Ok(Inherited::Stream(
            connector.duplicate(hyper_os::stream::CONNECTOR_RIGHTS)?,
        ));
    }
    let (pipe, rights) = match which {
        StandardStream::Input => (
            hyper_rt::process::stdin()?,
            hyper_service::stdio::STANDARD_INPUT_CONTRACT,
        ),
        StandardStream::Output => (
            hyper_rt::process::stdout()?,
            hyper_service::stdio::STANDARD_OUTPUT_CONTRACT,
        ),
        StandardStream::Error => (
            hyper_rt::process::stderr()?,
            hyper_service::stdio::STANDARD_ERROR_CONTRACT,
        ),
    };
    Ok(Inherited::Pipe(pipe.duplicate(rights.required_rights())?))
}

fn read_script(path: &str) -> Result<String, Error> {
    let mut source = String::new();
    std::fs::File::open(path)
        .map_err(Error::Io)?
        .take(hyper_shell::syntax::MAX_SCRIPT_BYTES as u64 + 1)
        .read_to_string(&mut source)
        .map_err(Error::Io)?;
    Ok(source)
}

fn run_script(
    shell: &mut Shell<launch::NativeBackend>,
    source: &str,
    check: bool,
) -> Result<ExitCode, Error> {
    if check {
        hyper_shell::syntax::parse(source).map_err(|e| Error::Message(e.to_string()))?;
        return Ok(ExitCode::SUCCESS);
    }
    let code = shell.execute(source).map_err(Error::Message)?;
    Ok(exit_code(match shell.flow {
        Flow::Exit(code) => code,
        _ => code,
    }))
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from((code & 255) as u8)
}

fn interactive_shell(shell: &mut Shell<launch::NativeBackend>) -> Result<ExitCode, Error> {
    let mut history = hyper_shell::history::History::load();
    let mut editor = Editor::default();
    editor.set_history(history.entries());
    let mut source = String::new();
    let mut renderer = interactive::Renderer::default();
    if let Some(terminal) = shell.backend.authorities.terminal.as_mut() {
        let (columns, _) = terminal.size()?;
        renderer.columns(columns);
    }
    write_terminal(READY_MESSAGE)?;
    write_terminal(PROMPT)?;
    write_terminal(b"\x1b[?2004h")?;
    let mut input_chunk = [0; INPUT_CHUNK_BYTES];
    loop {
        let count = std::io::stdin().read(&mut input_chunk).map_err(Error::Io)?;
        if count == 0 {
            write_terminal(b"\x1b[?2004l")?;
            return Ok(exit_code(shell.state.status));
        }
        for byte in input_chunk[..count].iter().copied() {
            let prompt = if source.is_empty() {
                "hyper-sh$ "
            } else {
                "> "
            };
            match editor.push(byte) {
                Action::None => {}
                Action::Echo(byte) => {
                    write_terminal(&[byte])?;
                    renderer.track(&editor, prompt);
                }
                Action::Backspace(removed) => {
                    write_terminal(renderer.backspace(&editor, prompt, removed).as_bytes())?;
                }
                Action::Redraw | Action::ClearScreen => {
                    if byte == 12 {
                        write_terminal(b"\x1b[2J\x1b[H")?;
                        renderer.reset();
                    }
                    write_terminal(renderer.draw(&editor, prompt).as_bytes())?;
                }
                Action::Complete => {
                    let line = std::str::from_utf8(editor.line()).unwrap_or("");
                    if let Some(completion) =
                        hyper_shell::completion::complete(line, editor.cursor(), &shell.state)
                    {
                        if !completion.choices.is_empty() {
                            write_terminal(b"\n")?;
                            for choice in completion.choices {
                                write_terminal(
                                    format!("{}  ", interactive::display(&choice)).as_bytes(),
                                )?;
                            }
                            write_terminal(b"\n")?;
                            renderer.reset();
                        }
                        editor.replace(completion.start, &completion.replacement);
                        write_terminal(renderer.draw(&editor, prompt).as_bytes())?;
                    } else {
                        write_terminal(b"\x07")?;
                    }
                }
                Action::Submit(line) => {
                    write_terminal(b"\x1b[?2004l\n")?;
                    renderer.reset();
                    let line = match String::from_utf8(line) {
                        Ok(line) => line,
                        Err(_) => {
                            source.clear();
                            shell.state.status = 2;
                            write_terminal(b"sh: command is not UTF-8\nhyper-sh$ \x1b[?2004h")?;
                            continue;
                        }
                    };
                    source.push_str(&line);
                    source.push('\n');
                    match hyper_shell::syntax::parse(&source) {
                        Err(error) if error.incomplete => {
                            renderer.prompt("> ");
                            write_terminal(b"> \x1b[?2004h")?;
                            continue;
                        }
                        Err(error) => {
                            shell.state.status = 2;
                            write_terminal(format!("sh: {error}\n").as_bytes())?;
                        }
                        Ok(_) => {
                            history.push(source.trim_end_matches('\n').to_owned());
                            shell.history = history.entries().to_vec();
                            if let Err(error) = shell.execute(&source) {
                                shell.state.status = 2;
                                write_terminal(format!("sh: {error}\n").as_bytes())?;
                            }
                            if shell.history.is_empty() {
                                history.clear();
                            }
                            // A missing or full /tmp never disables interactive history.
                            let _ = history.save();
                            editor.set_history(history.entries());
                            if let Flow::Exit(code) = shell.flow {
                                return Ok(exit_code(code));
                            }
                        }
                    }
                    source.clear();
                    write_terminal(PROMPT)?;
                    write_terminal(b"\x1b[?2004h")?;
                }
                Action::TooLong => {
                    source.clear();
                    renderer.reset();
                    shell.state.status = 2;
                    write_terminal(b"\nsh: command line is too long\nhyper-sh$ ")?;
                }
                Action::Cancel => {
                    source.clear();
                    renderer.reset();
                    shell.state.status = 130;
                    write_terminal(b"^C\nhyper-sh$ ")?;
                }
                Action::Exit if !source.is_empty() => {
                    source.clear();
                    write_terminal(b"\nsh: incomplete command\nhyper-sh$ ")?;
                }
                Action::Exit => {
                    write_terminal(b"\x1b[?2004l\n")?;
                    return Ok(exit_code(shell.state.status));
                }
            }
        }
    }
}

fn write_terminal(bytes: &[u8]) -> Result<(), Error> {
    let mut output = std::io::stdout().lock();
    output.write_all(bytes).map_err(Error::Io)?;
    // Flush prompts and individual key echoes before the next blocking wait.
    output.flush().map_err(Error::Io)
}

struct CommandAuthorities {
    services: Option<OwnedHandle<hyper_os::handle::CapabilityChannelObject>>,
    service_control: Option<OwnedHandle<hyper_os::handle::CapabilityChannelObject>>,
    journal: Option<OwnedHandle<hyper_os::handle::CapabilityChannelObject>>,
    root_directory: Directory,
    environment: Directory,
    current_directory: Directory,
    current_path: CanonicalPath,
    library_directory: Directory,
    loader: File,
    factory: OwnedHandle<TaskFactoryObject>,
    group: OwnedHandle<TaskGroupObject>,
    domain: OwnedHandle<ResourceDomainObject>,
    task_inspector: OwnedHandle<TaskInspectorObject>,
    object_inspector: OwnedHandle<ObjectInspectorObject>,
    memory_inspector: OwnedHandle<MemoryInspectorObject>,
    cpu_inspector: OwnedHandle<CpuInspectorObject>,
    vm_connection: Option<CapabilityChannel>,
    filesystem_control: Option<OwnedHandle<CapabilityChannelObject>>,
    terminal: Option<hyper_terminal::Terminal>,
}

#[derive(Debug)]
enum Error {
    OperatingSystem(OsError),
    Io(std::io::Error),
    InvalidCommand,
    Protocol,
    Message(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OperatingSystem(error) => write!(formatter, "Native operation failed: {error:?}"),
            Self::Io(error) => write!(formatter, "I/O failed: {error}"),
            Self::InvalidCommand => formatter.write_str("command launch failed"),
            Self::Protocol => formatter.write_str("protocol violation"),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl From<OsError> for Error {
    fn from(error: OsError) -> Self {
        Self::OperatingSystem(error)
    }
}

fn main() -> ExitCode {
    let args = hyper_shell::cli::Shell::parse();
    match hyper_rt::process::startup()
        .map_err(Error::from)
        .and_then(|mut startup| run(&mut startup, &args))
    {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "sh: {error}");
            ExitCode::from(2)
        }
    }
}
