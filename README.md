<!--
SPDX-FileCopyrightText: 2026 roolrz
SPDX-License-Identifier: Apache-2.0
-->

# shell

Native command interpreter and interactive shell.

Owns parsing, expansion, redirection, command execution and foreground session
coordination. Programs launch through the Native runtime and the capabilities delegated
to the shell; file resolution uses the caller’s namespace.

See the [implementation entry](src/main.rs) and [component declaration](component.json)
for the public interfaces, build dependencies and delivered artifacts.

## Usage

In a Native session with the required capabilities:

```sh
sh --help
sh -c "echo hello"
```

## Boundaries

The supported shell and arithmetic subsets are documented in this repository. Unquoted
positional arguments retain their boundaries before splitting; redirection does not
lexically erase symlink/.. traversal.

## Build and test

Use this checkout inside the [repo-managed workspace](https://github.com/HypeR-Proj/HypeR).
From this repository directory:

```sh
make
make test
```

An absolute `OUT=/path/to/out` selects a shared output directory. The module
entry delegates to build/system, which prepares the installed SDK and declared
dependencies. Native binaries require HypeR; host module checks cover the tests
selected by component.json. A standalone clone does not contain the workspace’s
other source repositories or Native SDK.

## Cooperative termination

Ctrl-C is a terminal event for the foreground job. The shell requests Terminate
for each registered member; it does not stop a kernel TaskGroup. Immediate
parents retain their unique Process supervisor handles. Other job participants
hold wait-only Process observers and request-only channels, never thread or
process management authority. Each parent applies Default, Ignore or Notify;
the shell's Default action is process termination. Notify lets an application
clean up and exit normally. Repeated Ctrl-C does not imply force escalation.

Foreground requests preserve normal application exit statuses, including Ignore
and successful Notify cleanup. An applied Default stop cancels the command list
with status 130. A notified shell also returns 130 at its next safe completion
point; a child returning 130 after a Terminate request carries this cooperative
interruption convention to its parent. An ordinary exit 130 without a request
does not cancel subsequent commands. Other nonzero statuses remain unchanged.

The standard shell launch path installs the `hyper-service::process::control`
connection and registers members through `process::job`. Registration and group
broadcast use separate capabilities; ordinary programs receive only the
registration connector. Nested shells additionally receive the group broadcast
capability so their foreground terminal leases can address the inherited job.
The parent registers a Process observer after `ProcessBuilder::start` returns;
this is not a guarantee that user code cannot execute before registration.
Programs that bypass this cooperative protocol, including the current Rust
`std::process::Command` shim, do not automatically gain job registration or a
supervisor reactor. A parent that does not process requests can leave its child
unresponsive to Ctrl-C. This is not a POSIX signal implementation.

The registry observes descendants independently of their original parent.
Parent loss makes its request route unavailable; it neither removes a surviving
member nor silently transfers control authority to the shell. Once direct
pipeline members finish, surviving registered descendants remain observable in
a background registry; this does not add `jobs`, `fg`, or `bg` commands. Empty
registries close their admission endpoints and join their workers.

Foreground terminal leases belong to the supervising shell, not the pipeline's
last consumer. Consumer exit restores presentation settings without revoking
other members' access. Closing the job event endpoint or explicit release ends
the lease. Events carry a lease identity and sequence, so stale job events cannot
terminate a subsequent command. At the prompt Ctrl-C clears editing state. Shell
builtin loops and standard-input reads service cooperative safe points; input
cancellation collects the original read completion before releasing ownership.
Filesystem operations and arbitrary blocking application calls are not made
asynchronously cancellable by installing a handler.
