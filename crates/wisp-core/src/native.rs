//! Generators Fig wrote in JavaScript and Wisp answers by reading a file.
//!
//! A `custom` generator carries no script, so nothing about it can be run; what
//! it would have produced is usually sitting in a file the shell can see
//! anyway. The owning argument in each command RON names one of these, and the
//! engine reaches for it when that argument's generators are JavaScript-only.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::generator::Suggestion;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Native {
    /// The `scripts` object of the nearest `package.json`.
    PackageJsonScripts,
    /// Targets declared by the makefile in the working directory.
    MakeTargets,
    /// Hosts from `~/.ssh/config` and `~/.ssh/known_hosts`.
    SshHosts,
    /// Commands the shell has already run.
    History,
}

impl Native {
    pub fn suggest(self, cwd: &Path, history_limit: usize) -> Vec<Suggestion> {
        match self {
            Self::PackageJsonScripts => package_json_scripts(cwd),
            Self::MakeTargets => make_targets(cwd),
            Self::SshHosts => ssh_hosts(),
            Self::History => history(history_limit),
        }
    }
}

fn suggestion(name: impl Into<String>, description: Option<String>) -> Suggestion {
    Suggestion {
        name: name.into(),
        description,
        insert: None,
    }
}

/// Walks up from the working directory the way a package manager does.
fn find_upwards(cwd: &Path, name: &str) -> Option<PathBuf> {
    let mut directory = Some(cwd);
    while let Some(current) = directory {
        let candidate = current.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        directory = current.parent();
    }
    None
}

fn package_json_scripts(cwd: &Path) -> Vec<Suggestion> {
    let Some(path) = find_upwards(cwd, "package.json") else {
        return Vec::new();
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&source) else {
        return Vec::new();
    };
    document
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .map(|scripts| {
            scripts
                .iter()
                .map(|(name, command)| {
                    suggestion(name.clone(), command.as_str().map(str::to_owned))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn make_targets(cwd: &Path) -> Vec<Suggestion> {
    let source = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .find_map(|name| std::fs::read_to_string(cwd.join(name)).ok());
    let Some(source) = source else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for line in source.lines() {
        // A target is a name at the start of a line, followed by a colon that
        // is not part of an assignment such as `CFLAGS := -g`.
        if line.starts_with([' ', '\t', '#', '.']) {
            continue;
        }
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if rest.starts_with('=') || name.is_empty() {
            continue;
        }
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character.is_alphanumeric() || "-_./".contains(character))
        {
            continue;
        }
        if seen.insert(name.to_owned()) {
            targets.push(suggestion(name, Some("Target".into())));
        }
    }
    targets
}

fn ssh_hosts() -> Vec<Suggestion> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    ssh_hosts_in(&home)
}

fn ssh_hosts_in(home: &Path) -> Vec<Suggestion> {
    let mut seen = HashSet::new();
    let mut hosts = Vec::new();

    if let Ok(config) = std::fs::read_to_string(home.join(".ssh/config")) {
        for line in config.lines() {
            let line = line.trim();
            let Some(rest) = line
                .strip_prefix("Host ")
                .or_else(|| line.strip_prefix("host "))
            else {
                continue;
            };
            for host in rest.split_whitespace() {
                // Patterns match hosts, they are not hosts themselves.
                if host.contains(['*', '?', '!']) {
                    continue;
                }
                if seen.insert(host.to_owned()) {
                    hosts.push(suggestion(host, Some("Configured host".into())));
                }
            }
        }
    }

    if let Ok(known) = std::fs::read_to_string(home.join(".ssh/known_hosts")) {
        for line in known.lines() {
            let Some(field) = line.split_whitespace().next() else {
                continue;
            };
            // Hashed entries say nothing a person can read.
            if field.starts_with('|') {
                continue;
            }
            for host in field.split(',') {
                let host = host.trim_start_matches('[');
                let host = host.split(']').next().unwrap_or(host);
                if host.is_empty() || !seen.insert(host.to_owned()) {
                    continue;
                }
                hosts.push(suggestion(host, Some("Known host".into())));
            }
        }
    }
    hosts
}

/// The most recent commands first, which is the order Fig's history template
/// shows them in.
fn history(limit: usize) -> Vec<Suggestion> {
    let path = std::env::var_os("HISTFILE").map(PathBuf::from).or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        [".zsh_history", ".bash_history"]
            .iter()
            .map(|name| home.join(name))
            .find(|candidate| candidate.is_file())
    });
    let Some(path) = path else {
        return Vec::new();
    };
    history_in(&path, limit)
}

fn history_in(path: &Path, limit: usize) -> Vec<Suggestion> {
    let Ok(source) = std::fs::read(path) else {
        return Vec::new();
    };
    let source = String::from_utf8_lossy(&source);
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for line in source.lines().rev() {
        // zsh writes `: <started>:<elapsed>;<command>` under EXTENDED_HISTORY.
        let command = line
            .strip_prefix(would_be_metadata(line))
            .unwrap_or(line)
            .trim();
        if command.is_empty() || !seen.insert(command.to_owned()) {
            continue;
        }
        entries.push(suggestion(command, Some("History".into())));
        if entries.len() >= limit {
            break;
        }
    }
    entries
}

fn would_be_metadata(line: &str) -> &str {
    if !line.starts_with(": ") {
        return "";
    }
    line.find(';').map_or("", |index| &line[..=index])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("wisp-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the scratch directory");
        directory
    }

    fn names(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions
            .iter()
            .map(|suggestion| suggestion.name.as_str())
            .collect()
    }

    #[test]
    fn package_scripts_come_from_the_nearest_manifest() {
        let root = scratch("package");
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).expect("create the nested directory");
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"build":"tsc","test":"vitest"}}"#,
        )
        .expect("write the manifest");

        let suggestions = package_json_scripts(&nested);
        assert_eq!(names(&suggestions), ["build", "test"]);
        assert_eq!(suggestions[0].description.as_deref(), Some("tsc"));
    }

    #[test]
    fn make_targets_skip_variables_and_recipes() {
        let root = scratch("make");
        std::fs::write(
            root.join("Makefile"),
            "CFLAGS := -g\n.PHONY: build\nbuild: deps\n\tcargo build\ntest-all:\n\tcargo test\n",
        )
        .expect("write the makefile");

        assert_eq!(names(&make_targets(&root)), ["build", "test-all"]);
    }

    #[test]
    fn ssh_hosts_come_from_the_config_and_known_hosts() {
        let home = scratch("ssh");
        std::fs::create_dir_all(home.join(".ssh")).expect("create .ssh");
        std::fs::write(
            home.join(".ssh/config"),
            "Host build deploy\n  User root\nHost *.internal\n",
        )
        .expect("write the config");
        std::fs::write(
            home.join(".ssh/known_hosts"),
            "[example.com]:22 ssh-rsa AAAA\n|1|hashed= ssh-rsa AAAA\n",
        )
        .expect("write known_hosts");

        let suggestions = ssh_hosts_in(&home);
        assert_eq!(names(&suggestions), ["build", "deploy", "example.com"]);
    }

    #[test]
    fn history_is_most_recent_first_without_zsh_metadata() {
        let root = scratch("history");
        let path = root.join("history");
        std::fs::write(
            &path,
            ": 1700000000:0;git status\n: 1700000001:0;cargo test\n: 1700000002:0;git status\n",
        )
        .expect("write the history");

        assert_eq!(
            names(&history_in(&path, 2_000)),
            ["git status", "cargo test"]
        );
    }
}
