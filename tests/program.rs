// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn mount_commands_use_the_system_environment_from_any_cwd() -> Result<(), Error> {
    for cwd in [
        CanonicalPath::root(),
        CanonicalPath::root().resolve("/data")?,
    ] {
        for name in ["mount", "umount"] {
            let program = Program::resolve(name, &cwd)?;
            assert_eq!(program.path, format!("/bin/{name}"));
            assert!(!program.is_shell());
        }
    }
    Ok(())
}

#[test]
fn ordinary_commands_have_one_system_location() -> Result<(), Error> {
    for name in ["sh", "ls", "ps", "vmm", "not-installed"] {
        let program = Program::resolve(name, &CanonicalPath::root())?;
        assert_eq!(program.path, format!("/bin/{name}"));
    }
    Ok(())
}

#[test]
fn explicit_paths_are_not_rewritten_into_a_different_environment() -> Result<(), Error> {
    let cwd = CanonicalPath::root().resolve("/data/tools")?;
    for (input, expected) in [
        ("./mount", "/data/tools/mount"),
        ("../vmm", "/data/vmm"),
        ("/system/bin/ps", "/system/bin/ps"),
        ("/boot/bin/sh", "/boot/bin/sh"),
    ] {
        let program = Program::resolve(input, &cwd)?;
        assert_eq!(program.path, expected);
        assert_eq!(program.authority, None);
    }
    Ok(())
}

#[test]
fn private_authorities_follow_selected_trusted_paths_not_basenames() -> Result<(), Error> {
    let cwd = CanonicalPath::root().resolve("/data")?;
    for (name, authority) in [
        ("mount", Authority::Filesystem),
        ("ps", Authority::Tasks),
        ("handle", Authority::Handles),
        ("free", Authority::Memory),
        ("top", Authority::Activity),
        ("vmm", Authority::VirtualMachines),
    ] {
        assert_eq!(Program::resolve(name, &cwd)?.authority, Some(authority));
        assert_eq!(
            Program::resolve(&format!("./{name}"), &cwd)?.authority,
            None
        );
    }
    assert_eq!(Program::resolve("/boot/bin/mount", &cwd)?.authority, None);
    assert_eq!(
        Program::resolve("/bin/vmm", &cwd)?.authority,
        Some(Authority::VirtualMachines)
    );
    Ok(())
}

#[test]
fn fallback_service_is_not_found_by_bare_name() -> Result<(), Error> {
    let program = Program::resolve("fallback-sh", &CanonicalPath::root())?;
    assert_eq!(program.path, "/bin/fallback-sh");
    assert!(!program.is_shell());
    assert!(Program::resolve("sh", &CanonicalPath::root())?.is_shell());
    Ok(())
}

#[test]
fn shell_delegation_is_limited_to_the_trusted_system_image() -> Result<(), Error> {
    let cwd = CanonicalPath::root().resolve("/data")?;
    for name in ["sh", "/bin/sh", "../bin/sh"] {
        assert!(Program::resolve(name, &cwd)?.is_shell());
    }
    for name in ["./sh", "/data/sh", "/boot/svc/fallback-sh", "/bin/sh-copy"] {
        assert!(!Program::resolve(name, &cwd)?.is_shell());
    }
    Ok(())
}
