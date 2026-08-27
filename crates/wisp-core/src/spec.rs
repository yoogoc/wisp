use std::{
    collections::HashMap,
    io::Read,
    ops::Range,
    path::Path,
    sync::{Arc, OnceLock},
};

use anyhow::{Context, bail};
use flate2::read::GzDecoder;
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
    #[serde(default)]
    pub arguments: Vec<ArgumentSpec>,
}

const fn spec_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubcommandSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default)]
    pub arguments: Vec<ArgumentSpec>,
    #[serde(default)]
    pub load_spec: Option<String>,
}

impl SubcommandSpec {
    pub fn matches(&self, value: &str) -> bool {
        self.name == value || self.aliases.iter().any(|alias| alias == value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionSpec {
    pub names: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub takes_value: bool,
    #[serde(default)]
    pub arguments: Vec<ArgumentSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgumentSpec {
    pub name: String,
    #[serde(default)]
    pub source: SuggestionSource,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub variadic: bool,
    #[serde(default)]
    pub imported_generator: Option<ImportedGenerator>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportedGenerator {
    #[serde(default)]
    pub script: Vec<String>,
    #[serde(default)]
    pub has_post_process: bool,
    #[serde(default)]
    pub has_custom: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum SuggestionSource {
    #[default]
    Files,
    Directories,
    Static(Vec<String>),
    Generator(String),
    /// Fig declared a dynamic JavaScript callback which has no safe Rust adapter yet.
    Unavailable,
}

#[derive(Debug, Clone, Default)]
pub struct SpecStore {
    specs: HashMap<String, Arc<StoredSpec>>,
    aliases: HashMap<String, String>,
}

#[derive(Debug)]
struct StoredSpec {
    value: OnceLock<CommandSpec>,
    compressed: Option<Range<usize>>,
}

impl StoredSpec {
    fn loaded(spec: CommandSpec) -> Self {
        let value = OnceLock::new();
        value
            .set(spec)
            .expect("new completion spec cell must be empty");
        Self {
            value,
            compressed: None,
        }
    }

    fn compressed(range: Range<usize>) -> Self {
        Self {
            value: OnceLock::new(),
            compressed: Some(range),
        }
    }

    fn get(&self) -> &CommandSpec {
        self.value.get_or_init(|| {
            let range = self
                .compressed
                .clone()
                .expect("an unloaded spec must have a data range");
            let bytes = &SPEC_DATA[range];
            let mut decoder = GzDecoder::new(bytes);
            let mut json = Vec::new();
            decoder
                .read_to_end(&mut json)
                .expect("built-in spec must decompress");
            let document = String::from_utf8(json).expect("built-in spec must be UTF-8 RON");
            ron::from_str(&document).expect("built-in spec must be valid RON")
        })
    }
}

/// Every RON document under `specs/`, gzip-compressed one by one and
/// concatenated by `build.rs`, alongside the `SPEC_INDEX` that locates them.
static SPEC_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/specs.ronpack"));

include!(concat!(env!("OUT_DIR"), "/spec_index.rs"));

impl SpecStore {
    pub fn builtins() -> Self {
        let mut store = Self::default();
        for &(id, command, offset, length) in SPEC_INDEX {
            store.aliases.insert(command.to_owned(), id.to_owned());
            store.specs.insert(
                id.to_owned(),
                Arc::new(StoredSpec::compressed(offset..offset + length)),
            );
        }
        store
    }

    fn insert_loaded(&mut self, id: String, aliases: Vec<String>, spec: CommandSpec) {
        self.aliases.insert(spec.command.clone(), id.clone());
        for alias in aliases {
            self.aliases.insert(alias, id.clone());
        }
        self.specs.insert(id, Arc::new(StoredSpec::loaded(spec)));
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
            self.insert_loaded(spec.command.clone(), Vec::new(), spec);
            loaded += 1;
        }
        Ok(loaded)
    }

    pub fn get(&self, command: &str) -> Option<&CommandSpec> {
        self.specs
            .get(command)
            .or_else(|| self.aliases.get(command).and_then(|id| self.specs.get(id)))
            .map(|spec| spec.get())
    }

    pub fn get_by_id(&self, id: &str) -> Option<&CommandSpec> {
        self.specs.get(id).map(|spec| spec.get())
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fig_index_contains_the_complete_snapshot() {
        let store = SpecStore::builtins();
        assert!(store.len() >= 1_484);
        assert!(store.get("az").is_some());
        assert!(store.get_by_id("az/2.53.0").is_some());
    }
}
