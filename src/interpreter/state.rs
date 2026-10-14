// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

use super::{Result, absolute};
use crate::syntax::Command;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Variable {
    pub value: String,
    pub exported: bool,
    pub readonly: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub variables: BTreeMap<String, Variable>,
    pub functions: BTreeMap<String, Command>,
    pub positional: Vec<String>,
    pub name: String,
    pub pid: u32,
    pub cwd: String,
    pub status: i32,
    pub pipeline_status: Vec<i32>,
    pub pipefail: bool,
    pub nounset: bool,
    pub trace: bool,
    pub last_duration_ns: u128,
}

impl Default for State {
    fn default() -> Self {
        let mut state = Self {
            variables: BTreeMap::new(),
            functions: BTreeMap::new(),
            positional: vec![],
            name: "sh".into(),
            pid: std::process::id(),
            cwd: "/".into(),
            status: 0,
            pipeline_status: vec![],
            pipefail: false,
            nounset: false,
            trace: false,
            last_duration_ns: 0,
        };
        for (name, value) in [
            ("PATH", "/bin"),
            ("IFS", " \t\n"),
            ("PWD", "/"),
            ("HOME", "/"),
        ] {
            state.variables.insert(
                name.into(),
                Variable {
                    value: value.into(),
                    exported: name != "IFS",
                    readonly: false,
                },
            );
        }
        state
    }
}

impl State {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(|v| v.value.as_str())
    }
    pub fn set(&mut self, name: &str, value: String) -> Result<()> {
        if !crate::syntax::identifier(name) {
            return Err(format!("{name}: invalid variable name"));
        }
        if value.len() > crate::syntax::MAX_SCRIPT_BYTES {
            return Err("variable size limit exceeded".into());
        }
        if let Some(variable) = self.variables.get_mut(name) {
            if variable.readonly {
                return Err(format!("{name}: readonly variable"));
            }
            variable.value = value;
        } else {
            self.variables.insert(
                name.into(),
                Variable {
                    value,
                    exported: false,
                    readonly: false,
                },
            );
        }
        Ok(())
    }
    pub fn environment(&self) -> Vec<String> {
        self.variables
            .iter()
            .filter(|(_, v)| v.exported)
            .map(|(k, v)| format!("{k}={}", v.value))
            .collect()
    }
    pub fn path(&self, value: &str) -> String {
        absolute(&self.cwd, value)
    }
    pub fn executable(&self, name: &str) -> Result<String> {
        if name.contains('/') {
            let path = self.path(name);
            if std::fs::metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
                return Err(format!("{name}: command not found"));
            }
            return Ok(path);
        }
        for directory in self.get("PATH").unwrap_or("").split(':') {
            let path = self.path(&format!(
                "{}/{name}",
                if directory.is_empty() { "." } else { directory }
            ));
            if std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                return Ok(path);
            }
        }
        Err(format!("{name}: command not found"))
    }
}
