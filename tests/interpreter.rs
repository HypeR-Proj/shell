// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use crate::interpreter::io::Port;
use crate::interpreter::{Backend, Invocation, Shell, Stage, State};
use std::cell::RefCell;
use std::rc::Rc;

struct NoProcesses;
impl Backend for NoProcesses {
    fn directory(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn pipeline(&mut self, stages: Vec<Stage>) -> Result<Vec<i32>, String> {
        let mut statuses = Vec::new();
        for stage in stages {
            match stage.invocation {
                Invocation::Shell(worker) => {
                    let mut shell = Shell::new(State::default(), NoProcesses);
                    shell.streams = stage.streams;
                    statuses.push(shell.worker(*worker)?);
                }
                Invocation::External { arguments, .. } => {
                    return Err(format!("unexpected external command: {arguments:?}"));
                }
            }
        }
        Ok(statuses)
    }
}

#[test]
fn nested_exec_keeps_extra_descriptors_without_leaking_temporary_stdout() -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("hyper-shell-scope-{}", std::process::id()));
    let extra = path.with_extension("extra");
    let source = format!(
        "f() {{ 'exec' 3>'{}'; echo inside; }}; f >'{}'; echo outside; echo retained >&3; exec 3>&-",
        extra.display(),
        path.display()
    );
    let result = output(&source);
    let temporary = std::fs::read_to_string(&path).map_err(|e| e.to_string());
    let retained = std::fs::read_to_string(&extra).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(extra);
    assert_eq!(result?, ("outside\n".into(), 0));
    assert_eq!(temporary?, "inside\n");
    assert_eq!(retained?, "retained\n");
    Ok(())
}

fn output(source: &str) -> Result<(String, i32), String> {
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut shell = Shell::new(State::default(), NoProcesses);
    shell.streams.set(1, Port::Capture(bytes.clone()));
    shell.streams.set(2, Port::Capture(bytes.clone()));
    let status = shell.execute(source)?;
    let result = String::from_utf8(bytes.borrow().clone()).map_err(|e| e.to_string())?;
    Ok((result, status))
}

#[test]
fn common_scripts_match_bash_output_and_status() -> Result<(), String> {
    let scripts = [
        "f() { true | { local value=inner; return 7; }; echo $?; }; f",
        "if exec 3<<<condition; then read -u 3 x; echo $x; fi",
        "if exec 3<<<fallback && false; then :; elif read -u 3 x; then echo $x; fi",
        "IFS=: read -r a b c <<< ':x::tail:'; printf '<%s><%s><%s>\\n' \"$a\" \"$b\" \"$c\"",
        "IFS=' :' read -r a b c <<< ' : x : tail : '; printf '<%s><%s><%s>\\n' \"$a\" \"$b\" \"$c\"",
        "read a b <<< 'one\\ two three'; printf '<%s><%s>\\n' \"$a\" \"$b\"",
        "exec 3<<<first; for n in 1 2; do read -u 3 value; echo $value; exec 3<<<second; done",
        "echo hello; false; echo $?; true",
        "false; echo ${?}; set -- a b; echo ${#} ${1} ${2} ${3-default}",
        "false; x=ok; echo $?; x=$(false); echo $?",
        "x=' a b '; printf '<%s>\\n' pre${x}post",
        "IFS=:; x='a::b:'; printf '<%s>\\n' $x",
        "set --; f() { echo $#; }; f \"$@\"",
        "x='two words'; export y=$x; echo \"$y\"",
        "case abc in 'a*') echo bad;; a*) echo good;; esac",
        "false && echo bad || echo good; ! true",
        "x='two words'; printf '<%s>\\n' \"$x\" $x ''",
        "set -- 'two words' '' last; printf '<%s>\\n' \"$@\"; echo $#; shift; echo $#",
        "if false; then echo bad; elif true; then echo good; else echo bad; fi",
        "for x in a b c; do echo \"$x\"; done",
        "i=0; while [ $i -lt 3 ]; do echo $i; i=$((i + 1)); done",
        "i=0; until [ $i -eq 3 ]; do i=$((i+1)); done; echo $i",
        "for x in 1 2 3; do if [ $x -eq 2 ]; then continue; fi; echo $x; done",
        "for x in a b; do for y in c d; do echo $x$y; break 2; done; done",
        "case abc in x*) echo bad;; a?c|z) echo yes;; *) echo bad;; esac",
        "f() { local x=inner; echo \"$1:$x\"; return 7; }; x=outer; f arg; echo \"$?:$x\"",
        "x=outer; (x=inner; echo $x); echo $x",
        "x=$(printf 'first\\nlast\\n\\n'); printf '<%s>\\n' \"$x\"",
        "x=outer; echo $(x=inner; echo $x); echo $x",
        "unset x; echo ${x:-default}; echo ${x:=set}; echo ${x:+present}; echo ${#x}",
        "printf '%s\\n' one two three",
        "echo $((2+3*4)); echo $((1<<4)); echo $((0 && 1)); echo $((1 || 0))",
        "read -r x <<'END'\nhello world\nEND\nprintf '<%s>\\n' \"$x\"",
        "x=world; read -r line <<END\nhello $x\nEND\necho \"$line\"",
        "read -r x <<< 'a b'; echo \"$x\"",
        "{ echo one; echo two; }",
        "f() { echo $1; }; f 'quoted;word'",
        "set --; printf 'x%sx\\n' \"$@\"",
    ];
    for source in scripts {
        let expected = std::process::Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-c", source])
            .output()
            .map_err(|e| e.to_string())?;
        let (actual, status) = output(source)?;
        let expected_text = String::from_utf8_lossy(&expected.stdout);
        assert_eq!(actual, expected_text, "{source}");
        assert_eq!(status, expected.status.code().unwrap_or(-1), "{source}");
    }
    Ok(())
}

#[test]
fn empty_positional_expansion_preserves_explicit_empty_arguments() -> Result<(), String> {
    // Older host Bash releases drop preceding empty quotes here. Pin the
    // intended argument semantics rather than inheriting that oracle bug.
    let (text, status) = output(
        "set --; f() { echo $#; }; x=; f \"$@\"; f ''\"$@\"; f \"$@\"''; f \"$x$@\"; f \"$@$@\"",
    )?;
    assert_eq!(text, "0\n1\n1\n1\n0\n");
    assert_eq!(status, 0);
    Ok(())
}

#[test]
fn redirection_aliases_order_closing_and_restoration() -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("hyper-shell-redir-{}", std::process::id()));
    let source = format!(
        "{{ echo out; echo err >&2; }} >'{}' 2>&1; echo terminal; exec 3<'{}'; read -u 3 a; read -u 3 b; echo \"$a:$b\"; exec 3<&-",
        path.display(),
        path.display()
    );
    let result = output(&source);
    let disk = std::fs::read_to_string(&path).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&path);
    assert_eq!(result?, ("terminal\nout:err\n".into(), 0));
    assert_eq!(disk?, "out\nerr\n");
    Ok(())
}

#[test]
fn invalid_and_incomplete_syntax_are_distinct() {
    for source in [
        "if true; then",
        "echo 'open",
        "cat <<END\nbody\n",
        "echo foo |",
        "f() {",
        "echo $(echo hi",
    ] {
        assert!(
            crate::syntax::parse(source).is_err_and(|e| e.incomplete),
            "{source}"
        );
    }
    for source in [
        "cat &",
        "echo > |",
        "else",
        "echo )",
        "for 123 in a; do true; done",
        "echo 128>file",
        "while do echo bad; done",
        "if then echo bad; fi",
    ] {
        assert!(
            crate::syntax::parse(source).is_err_and(|e| !e.incomplete),
            "{source}"
        );
    }
}

#[test]
fn diagnostics_obey_partial_redirection_and_prefix_variables_restore() -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("hyper-shell-errors-{}", std::process::id()));
    let result = output(&format!(
        "printf '%z' 2>'{}'; echo STATUS=$?; echo never 2>>'{}' >/missing-shell-dir/output; echo STATUS=$?; x=old; readonly y=old; x=new y=new echo never; echo X=$x",
        path.display(),
        path.display()
    ));
    let error = std::fs::read_to_string(&path).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(path);
    let (text, status) = result?;
    assert!(text.starts_with("STATUS=1\nSTATUS=1\n"), "{text}");
    assert!(text.ends_with("X=old\n"), "{text}");
    assert!(!text.contains("never"));
    assert_eq!(status, 0);
    let error = error?;
    assert!(
        error.contains("printf") && error.contains("missing-shell-dir"),
        "{error}"
    );
    Ok(())
}

#[test]
fn readonly_and_function_scopes_do_not_escape() -> Result<(), String> {
    let (text, status) =
        output("readonly x=outer; x=inner; echo $x; f() { local y=inner; }; f; echo ${y-unset}")?;
    assert!(text.contains("readonly variable"));
    assert!(text.ends_with("outer\nunset\n"));
    assert_eq!(status, 0);
    Ok(())
}

#[test]
fn directory_globs_keep_the_trailing_slash_and_exclude_files() -> Result<(), String> {
    let base = std::env::temp_dir().join(format!("hyper-shell-glob-{}", std::process::id()));
    std::fs::create_dir_all(base.join("directory")).map_err(|e| e.to_string())?;
    std::fs::write(base.join("file"), b"").map_err(|e| e.to_string())?;
    // Use an absolute wildcard; the test must not depend on the host cwd.
    let actual = output(&format!("printf '%s\\n' {}/*/", base.display()));
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(actual?, (format!("{}/directory/\n", base.display()), 0));
    Ok(())
}
