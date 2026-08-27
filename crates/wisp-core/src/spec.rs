use std::{collections::HashMap, path::Path};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    #[serde(default = "spec_version")]
    pub version: u32,
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
}

const fn spec_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubcommandSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default)]
    pub arguments: Vec<ArgumentSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionSpec {
    pub names: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub takes_value: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgumentSpec {
    pub name: String,
    #[serde(default)]
    pub source: SuggestionSource,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum SuggestionSource {
    #[default]
    Files,
    Directories,
    Static(Vec<String>),
    Generator(String),
}

#[derive(Debug, Clone, Default)]
pub struct SpecStore {
    specs: HashMap<String, CommandSpec>,
}

impl SpecStore {
    pub fn builtins() -> Self {
        let mut store = Self::default();
        for source in [
            include_str!("../../../specs/git.ron"),
            include_str!("../../../specs/cargo.ron"),
            include_str!("../../../specs/docker.ron"),
        ] {
            let spec: CommandSpec = ron::from_str(source).expect("built-in spec must be valid");
            store.specs.insert(spec.command.clone(), spec);
        }
        store
    }

    pub fn load_dir(&mut self, directory: &Path) -> anyhow::Result<usize> {
        if !directory.exists() {
            return Ok(0);
        }
        let mut loaded = 0;
        for entry in std::fs::read_dir(directory).context("read specs directory")? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("ron") {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("read completion spec {}", path.display()))?;
            let spec: CommandSpec = ron::from_str(&source)
                .with_context(|| format!("parse completion spec {}", path.display()))?;
            if spec.version != 1 {
                bail!(
                    "unsupported spec version {} in {}",
                    spec.version,
                    path.display()
                );
            }
            self.specs.insert(spec.command.clone(), spec);
            loaded += 1;
        }
        Ok(loaded)
    }

    pub fn get(&self, command: &str) -> Option<&CommandSpec> {
        self.specs.get(command)
    }
}
