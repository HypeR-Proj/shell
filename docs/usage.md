<!--
SPDX-FileCopyrightText: 2026 roolrz
SPDX-License-Identifier: Apache-2.0
-->

# Shell and text filtering

The formal shell is `app/shell`, installed as `/bin/sh`. It provides
interactive editing and a shell language with common Bourne/Bash constructs,
using Native process and stream capabilities. It does not implement the full
Bash language, job control, or Linux process/signal/descriptor ABIs.

`bootstrap/stage1/fallback-sh` is a separate, statically linked recovery service,
installed as `/svc/fallback-sh` in the initial ramdisk and retained at
`/boot/svc/fallback-sh` after root switch, until system init retires the boot archive. It has no clap dependency, command-line
options, history, pipelines or file redirection. Its builtins are `cd`, `pwd`,
`help` and `exit`; bounded line editing and quoted executable arguments remain.
It can launch recovery tools and explicitly selected static programs when those
are accessible; it does not delegate formal loaders or shell authority. The `svc` directories are never searched for bare command names.

Stage-1 init launches fallback directly only for failures before supervision
handoff. Its input and output are raw byte streams from `console-raw`, with no
terminal settings or line discipline. After ownership transfers, formal init
starts `session-manager`, which launches `terminal-service` and the formal
shell. Terminal or shell exit retires the old session and starts a fresh one,
with bounded retries and a delay. Failures after handoff never select bootstrap
recovery. See [Native terminals](https://hyper-proj.github.io/terminal-service/docs/terminal.html) for ownership and restart behavior.

The formal shell searches `PATH`, initially `/bin`. Explicit
paths resolve against its current directory. The fallback shell keeps its fixed
boot-environment lookup for `mount` and `umount`. In the
[default deployment](https://hyper-proj.github.io/bootstrap/docs/boot-layout.html), the independent system filesystem is the root.
Fallback launches of boot tools do not open the system library directory, so
storage failure cannot prevent static recovery commands. Formal commands use the
formal dynamic loader for both static and dynamic executables. Use `help` for shell syntax and
`APP --help` for ordinary application options.

The trusted `/bin/sh` executable can be launched from another formal shell. It
receives the formal image environment, diagnostic authority and available
filesystem/VM client connectors with rights to delegate them to its own tools.
Ordinary commands, including copies at other paths, receive only their
command-specific grants. Recovery mount commands do not commit a new handoff;
a new formal startup attempt requires a reboot.

## Editing and history

Arrow keys move the UTF-8 cursor or recall previous commands. Home/End and
Ctrl-A/E move to either end; Backspace/Delete remove adjacent characters.
Ordinary Backspace erases the deleted cells or repaints only the shifted suffix,
preserving the prompt and unchanged prefix. Wrap boundaries and wide characters
use cursor positioning; combining characters and composite glyphs retain a full redraw.
Ctrl-U/K kill the prefix/suffix, Ctrl-W kills the preceding word, and Ctrl-Y
reinserts killed text. Ctrl-L clears/redraws the screen. Ctrl-C cancels the
input being edited, including unfinished multiline syntax. It does not terminate
a running command; process interruption and job control are not implemented. Ctrl-D exits at an empty
prompt. Unsupported escape sequences do not become executable text.

Tab completes builtins, functions, commands in `PATH`, and file/directory names,
quoting inserted paths as needed. Ambiguous completion lists candidates and
inserts their common prefix. Path candidates show only the next component's name;
the inserted text retains the full path. `cd` arguments complete directories,
including symbolic links to directories. Ctrl-R searches history backwards; repeat Ctrl-R
for an older match, Enter to accept it for editing, or Ctrl-G to restore the
original draft. Bracketed paste inserts embedded newlines without executing
until Enter. Incomplete quotes, compound commands, and here-documents continue
at the `> ` prompt.

History is bounded to 256 entries and stored **only** in
`/tmp/hyper-sh.history`, with a 256 KiB file limit. It can survive a shell/session
restart in the same boot, but the default RAM-backed `/tmp` is cleared on reboot.
There is no persistent history path or home-directory fallback. Missing/unwritable
`/tmp` leaves in-memory history usable. Consecutive duplicates, blank lines,
rejected overlong lines and commands starting with whitespace are omitted.
`history` lists entries; `history -c` clears them. A leading space keeps a command
out of history, but does not hide its terminal echo or trace output.

## Commands and execution state

The shell supports interactive use, `sh SCRIPT [ARG...]`,
`sh -c 'COMMANDS' [NAME [ARG...]]`, and stdin scripts. `sh -n SCRIPT` checks
syntax without executing. Noninteractive shells emit neither prompts nor
session-readiness messages. Positional arguments use `$0`, `$1`, `${10}`, `$#`,
`"$@"` and `"$*"`.

Commands compose with `;`, newlines, `&&`, `||`, `!`, pipelines, `{ ...; }` groups,
and `( ... )` subshell scopes. Control structures include `if`/`elif`/`else`,
`for NAME in WORDS`, `while`, `until`, and `case` with `;;` arms. Functions use
`name() { ...; }`, have positional arguments and `local` variables, and return
with `return STATUS`. `break` and `continue` accept an optional loop count.
`source` (or `.`) evaluates a script in the caller's shell state.

```sh
report() {
    local path=$1
    if test -f "$path"; then
        cat "$path"
    else
        printf 'missing: %s\n' "$path" >&2
        return 1
    fi
}
for file in /tmp/*.txt; do report "$file"; done
```

Assignments, `export`, `readonly`, `unset`, `shift` and `set --` manage shell
state. Single quotes are literal; double quotes permit expansion without field
splitting or pathname expansion. Unquoted substitutions are split by `IFS` and
then globbed. `*`, `?` and bracket patterns are supported; unmatched pathname
patterns remain literal. Dotfiles require an explicit leading dot. `$(...)`
captures stdout and removes trailing newlines. `$((...))` provides integer
arithmetic, comparisons, bitwise and logical operators. Parameter expansion
includes `${name:-default}`, `${name:=value}`, `${name:+alternate}`,
`${name:?error}` and `${#name}`. The colon selects unset-or-empty rather than
unset-only handling. Comments start with `#` at a word boundary.

`$?` is the previous command's exit status. A pipeline uses its last stage's
status; `set -o pipefail` selects its rightmost nonzero status instead.
`${PIPESTATUS[@]}` reports the individual stage statuses. `status` prints the
last status, elapsed time and pipeline results. `set -x` traces expanded
commands; `set -u` diagnoses unset parameters. `set +x`, `set +u` and
`set +o pipefail` turn those options off. `exit N` returns N modulo 256.

## Pipelines and redirection

```sh
ls -1 /bin | grep '^c'
cat /data/vms.json | grep -n image
echo first > /tmp/notes
echo second >> /tmp/notes
cat /missing > /tmp/all-output 2>&1
read -r value <<< 'hello world'
cat <<'END' > /tmp/literal
A dollar sign here is literal: $value
END
exec 3< /tmp/notes
read -u 3 first
exec 3<&-
```

`|` connects stdout to the next command's stdin. All stages start before waiting
for completion. Plain external stages connect directly through byte channels.
Unredirected standard output retains its inherited transport rather than passing
through independent relays in every nested shell. Functions, builtins and
compound stages use isolated shell processes with their
own variable state. The shell drains output and releases the terminal lease even
if a later start or I/O operation fails. A downstream early exit closes its pipe
rather than leaving an upstream producer blocked indefinitely.

| Operator | Meaning |
| --- | --- |
| `<`, `>`, `>>` | Read, create/truncate, or append a file. |
| `<>`, `>|` | Open read/write, or explicitly truncate. |
| `n>&m`, `n<&m` | Alias descriptor `m`, sharing its open description. |
| `n>&-`, `n<&-` | Close a shell descriptor. |
| `n>&m-`, `n<&m-` | Move a shell descriptor. |
| `&>`, `&>>` | Redirect both stdout and stderr. |
| `|&` | Pipe stdout and stderr, applying `2>&1` after explicit redirects. |
| `<<`, `<<-` | Here-document; `<<-` strips leading tabs. Quoted delimiters suppress expansion. |
| `<<<` | Expanded here-string followed by a newline. |

Redirects apply **left to right**, after pipe wiring. Consequently `>file 2>&1`
and `2>&1 >file` have different destinations. Earlier file opens/truncations
still occur when a later redirect replaces that stream. A file target must
expand to exactly one word. Redirection also applies to builtins, functions and
compound commands; it is restored after that command. `exec` without a command
keeps redirections for subsequent shell commands.

Descriptors 0 through 63 are shell-local stream names. Native programs inherit
the three standard streams; this does not introduce Unix fd numbers into the
kernel or SDK. Extra descriptors inherited by a shell pipeline worker use a
private request/reply channel, preserving shared file offsets without read-ahead
by an idle reader. Redirected standard input also uses request-sized stream reads,
so a program that never reads cannot consume a later command's input.
File adapters use bounded buffers, not whole-output capture;
file I/O on the relay thread remains synchronous.

A foreground pipeline receives a scoped canonical terminal lease. Its final
stage owns terminal control and an optional keyboard connector independent of
stdin, keeping redirected data separate from interactive keystrokes.
Earlier stages use ordinary streams under that lease. Single commands follow
the same rule.
`read` temporarily selects canonical input and restores the shell's raw editing
profile. Ctrl-D after a partial canonical line makes it readable; at an empty
line it supplies one zero-length read. Commands selecting raw mode receive
Ctrl-D as a byte. There is no foreground/background job table or signal-driven
suspend/resume.

## Scope and limits

This is a Native shell with a defined language subset, not a claim of full Bash
compatibility. Background `&`, `jobs`/`fg`/`bg`, process substitution, arrays,
backtick substitution, arithmetic assignment/C-style `for`, `[[ ... ]]`, aliases,
and `exec COMMAND` image replacement are not implemented. Use `$(...)`, `test`,
and ordinary loops for supported equivalents. `printf` supports strings, bytes,
quoted words and integer formats, including bounded width/precision, but not
floating-point formatting.

An editable line is limited to 4 KiB, scripts and command-substitution output to
1 MiB, expansion results to 4096 words, and pipelines to 32 stages. Parsing,
expansion and function/source nesting are bounded. Native process arguments and
environments also retain the SDK's per-entry and aggregate limits. Worker state
travels over a private channel rather than temporary files, argv, or stdin.

`make app-test APP_TEST_PACKAGE=hyper-shell` checks the parser, evaluator and
editor, including differential cases against `/bin/bash` on the host.
`make test-shell ARCH=aarch64` verifies pipelines, script/function state,
redirection, volatile history and editing through actual Native processes;
`make test-console` and `make test-terminal` cover terminal ownership/typeahead.

## echo and cat

`echo -n TEXT` omits the trailing newline. `-e` interprets `\\`, `\a`, `\b`,
`\e`, `\f`, `\n`, `\r`, `\t`, `\v`, `\0NNN` (up to three octal digits)
and `\xHH` (up to two hex digits). `\c` stops all remaining output, including
the newline. `-E` restores literal backslashes; the last `-e`/`-E` wins. Unknown
escapes remain literal. Options precede the first text operand; unknown options
such as `--help` are text. Use `--` to make a leading `-n` or `-e` an operand.

`cat -n` numbers every line, `-b` numbers only nonempty lines and overrides `-n`,
and `-s` squeezes repeated empty lines. Numbering and empty-line state continue
across file boundaries, including an input without a final newline. With no
options, data passes through unchanged, including binary bytes.

## grep

`grep PATTERN [FILE ...]` reads stdin when no file is supplied; `-` explicitly
selects stdin. Useful options are:

- `-i`: ignore ASCII case; `-v`: select nonmatching lines.
- `-n`: show line numbers; `-c`: count selected lines per input.
- `-l`: print names of matching inputs; `-q`: stop after a match, without output.
- `-m NUM`: stop after NUM selected lines per input (`0` reads no lines).
- `-x`: require a whole-line match; `--line-buffered`: flush each selected line.
- `-F`: match literal text; `-e PATTERN`: supply multiple alternative patterns.
- `-H` / `-h`: force / suppress filename prefixes (automatic for multiple files).

Patterns use the Rust regex crate's byte-oriented syntax, including `^`, `$`,
character classes and alternation. They are not POSIX basic regular expressions;
backreferences and lookaround are not supported. Input need not be UTF-8, and
matching excludes each line's trailing newline. A missing final newline is
added when printing a selected line.

Exit statuses are 0 for a match, 1 for no match, and 2 for an error. `-q` returns
0 once a match is found even if an earlier input failed. Use `--help` for help;
`-h` is reserved for suppressing filenames.
