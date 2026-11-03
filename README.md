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
