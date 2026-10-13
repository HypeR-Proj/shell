// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new(name: &str) -> std::io::Result<Self> {
        let root =
            std::env::temp_dir().join(format!("hyper-completion-{name}-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }

    fn state(&self) -> State {
        State {
            cwd: self.0.to_string_lossy().into_owned(),
            ..State::default()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn lists_leaf_names_but_inserts_the_full_path() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("labels")?;
    let directory = fixture.0.join("boot/etc/hyper");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("board.json"), b"")?;
    std::fs::write(directory.join("services.json"), b"")?;
    let state = fixture.state();

    let line = "cat boot/etc/hyper/";
    let result = complete(line, line.len(), &state).ok_or("missing completion")?;
    assert_eq!(result.choices, ["board.json", "services.json"]);
    assert_eq!(result.replacement, "boot/etc/hyper/");

    let line = "cat boot/etc/hyper/b";
    let result = complete(line, line.len(), &state).ok_or("missing completion")?;
    assert!(result.choices.is_empty());
    assert_eq!(result.replacement, "boot/etc/hyper/board.json ");
    assert!(complete("cd boot/etc/hyper/", 18, &state).is_none());
    Ok(())
}

#[test]
fn directory_completion_filters_arguments_but_not_redirection()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("directories")?;
    let directory = fixture.0.join("nested space");
    std::fs::create_dir(&directory)?;
    std::fs::create_dir(directory.join("alpha"))?;
    std::fs::create_dir(directory.join("beta"))?;
    std::fs::write(directory.join("file"), b"")?;
    let state = fixture.state();

    for line in ["cd 'nested space/", "echo ignored && cd -- 'nested space/"] {
        let result = complete(line, line.len(), &state).ok_or("missing completion")?;
        assert_eq!(result.choices, ["alpha/", "beta/"]);
    }
    let line = "cd >'nested space/";
    let result = complete(line, line.len(), &state).ok_or("missing completion")?;
    assert_eq!(result.choices, ["alpha/", "beta/", "file"]);

    let line = "cd 'nested space/a";
    let result = complete(line, line.len(), &state).ok_or("missing completion")?;
    assert_eq!(
        shlex::split(&result.replacement),
        Some(vec!["nested space/alpha/".into()])
    );
    assert!(!result.replacement.ends_with(' '));
    Ok(())
}

#[test]
#[cfg(unix)]
fn directory_symlinks_remain_traversable_completion_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("symlinks")?;
    std::fs::create_dir(fixture.0.join("target"))?;
    std::os::unix::fs::symlink("target", fixture.0.join("link"))?;
    let line = "cd li";
    let result = complete(line, line.len(), &fixture.state()).ok_or("missing completion")?;
    assert_eq!(result.replacement, "link/");
    Ok(())
}
