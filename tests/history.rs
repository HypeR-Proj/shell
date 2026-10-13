// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn bounded_multiline_history_and_corrupt_file_recovery() -> std::io::Result<()> {
    let path = std::env::temp_dir().join(format!("hyper-history-{}", std::process::id()));
    let mut history = History::default();
    for n in 0..MAX_ENTRIES + 4 {
        history.push(format!("echo {n}"));
    }
    history.push(" leading-space-private".into());
    assert_eq!(history.entries().len(), MAX_ENTRIES);
    assert_eq!(history.entries()[0], "echo 4");
    history.push("echo one\necho two".into());
    history.push("echo one\necho two".into());
    history.save_path(&path)?;
    assert_eq!(History::load_path(&path).entries(), history.entries());
    history.clear();
    history.save_path(&path)?;
    assert!(History::load_path(&path).entries().is_empty());
    std::fs::write(&path, b"broken JSON")?;
    assert!(History::load_path(&path).entries().is_empty());
    std::fs::remove_file(path)
}

#[test]
#[cfg(unix)]
fn history_never_follows_a_persistent_file_symlink() -> std::io::Result<()> {
    let root = std::env::temp_dir().join(format!("hyper-history-link-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let persistent = root.join("persistent");
    let link = root.join("volatile");
    std::fs::write(&persistent, b"[\"secret\"]")?;
    std::os::unix::fs::symlink(&persistent, &link)?;
    assert!(History::load_path(&link).entries().is_empty());
    let mut history = History::default();
    history.push("echo local".into());
    history.save_path(&link)?;
    assert_eq!(std::fs::read(&persistent)?, b"[\"secret\"]");
    assert_eq!(History::load_path(&link).entries(), history.entries());
    std::fs::remove_dir_all(root)
}
