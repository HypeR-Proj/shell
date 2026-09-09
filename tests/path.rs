// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::CanonicalPath;

#[test]
fn resolves_parent_components_without_escaping_shell_root() {
    let root = CanonicalPath::root();
    let path = root
        .resolve("/usr/bin")
        .and_then(|path| path.resolve("../lib"));
    assert!(path.is_ok());
    let Ok(path) = path else {
        return;
    };
    assert_eq!(path.as_str(), Ok("/usr/lib"));

    let root_again = root.resolve("../../..");
    assert!(root_again.is_ok());
    let Ok(root_again) = root_again else {
        return;
    };
    assert_eq!(root_again.as_str(), Ok("/"));
}

#[test]
fn keeps_native_path_limits_and_rejects_nul() {
    let root = CanonicalPath::root();
    assert!(root.resolve("").is_err());
    assert!(root.resolve("a\0b").is_err());
    assert!(
        root.resolve(&"x".repeat(hyper_os::fs::MAX_NAME_BYTES + 1))
            .is_err()
    );
    let path = vec![
        "x".repeat(hyper_os::fs::MAX_NAME_BYTES);
        hyper_os::fs::MAX_PATH_BYTES / hyper_os::fs::MAX_NAME_BYTES + 1
    ]
    .join("/");
    assert!(root.resolve(&path).is_err());
}

#[test]
fn normalizes_repeated_separators_and_dot_components() {
    let path = CanonicalPath::root().resolve("//bin/./tools/");
    assert!(path.is_ok());
    let Ok(path) = path else {
        return;
    };
    assert_eq!(path.as_str(), Ok("/bin/tools"));
}
