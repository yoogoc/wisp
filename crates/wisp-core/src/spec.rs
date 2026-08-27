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

/// A command's completion spec, imported from a Fig `Subcommand` at the root of
/// a spec module. Every field a Fig spec can carry declaratively is kept;
/// whatever Fig expressed as a JavaScript callback is kept as a `has_*` flag so
/// the engine knows the difference between "nothing here" and "not expressible
/// without a JavaScript runtime".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandSpec {
    pub version: u32,
    pub command: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub subcommands: Vec<SubcommandSpec>,
    pub options: Vec<OptionSpec>,
    pub arguments: Vec<ArgumentSpec>,
    pub additional_suggestions: Vec<SuggestionSpec>,
    pub requires_subcommand: bool,
    pub filter_strategy: FilterStrategy,
    pub parser_directives: Option<ParserDirectives>,
    pub has_generate_spec: bool,
    pub load_spec: Option<String>,
    pub has_dynamic_load_spec: bool,
    pub load_spec_inline: Option<Box<SubcommandSpec>>,
    pub presentation: Presentation,
    /// The source module held no spec -- a shared helper or data module that
    /// keeps its id so `loadSpec` references still resolve.
    pub placeholder: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SubcommandSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub subcommands: Vec<SubcommandSpec>,
    pub options: Vec<OptionSpec>,
    pub arguments: Vec<ArgumentSpec>,
    pub additional_suggestions: Vec<SuggestionSpec>,
    pub requires_subcommand: bool,
    pub filter_strategy: FilterStrategy,
    pub parser_directives: Option<ParserDirectives>,
    pub has_generate_spec: bool,
    pub load_spec: Option<String>,
    pub has_dynamic_load_spec: bool,
    pub load_spec_inline: Option<Box<SubcommandSpec>>,
    pub presentation: Presentation,
}

impl SubcommandSpec {
    pub fn matches(&self, value: &str) -> bool {
        self.name == value || self.aliases.iter().any(|alias| alias == value)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OptionSpec {
    pub names: Vec<String>,
    pub description: Option<String>,
    pub arguments: Vec<ArgumentSpec>,
    /// The option stays available to every descendant subcommand.
    pub persistent: bool,
    pub required: bool,
    pub repeat: Repeat,
    /// The option's value must follow a separator rather than a space, as in
    /// `--message="text"`.
    pub requires_separator: Option<String>,
    /// Names this option cannot appear alongside.
    pub exclusive_on: Vec<String>,
    /// Names that must already be present for this option to apply.
    pub depends_on: Vec<String>,
    pub presentation: Presentation,
}

impl OptionSpec {
    pub fn matches(&self, value: &str) -> bool {
        self.names.iter().any(|name| name == value)
    }

    pub fn takes_value(&self) -> bool {
        !self.arguments.is_empty()
    }
}

/// How often an option may be repeated on one command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Once,
    Many,
    Times(u32),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArgumentSpec {
    pub name: String,
    pub description: Option<String>,
    /// Fixed values Fig lists for this argument.
    pub suggestions: Vec<SuggestionSpec>,
    /// Fig's prebuilt generators: paths, folders, history, help.
    pub template: Vec<Template>,
    pub generators: Vec<GeneratorSpec>,
    pub optional: bool,
    pub variadic: bool,
    pub options_can_break_variadic: bool,
    pub filter_strategy: FilterStrategy,
    pub suggest_current_token: bool,
    /// The argument is itself a command, so completion restarts from it.
    pub is_command: bool,
    pub is_script: bool,
    pub is_module: Option<String>,
    pub debounce: bool,
    pub default: Option<String>,
    pub load_spec: Option<String>,
    pub has_dynamic_load_spec: bool,
    pub load_spec_inline: Option<Box<SubcommandSpec>>,
    pub dangerous: bool,
    pub parser_directives: Option<ParserDirectives>,
}

impl ArgumentSpec {
    /// Whether anything at all can be suggested for this argument.
    pub fn is_empty(&self) -> bool {
        self.suggestions.is_empty()
            && self.template.is_empty()
            && self.generators.is_empty()
            && self.load_spec.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SuggestionSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub kind: Option<SuggestionType>,
    pub presentation: Presentation,
}

/// What a suggestion stands for, straight from Fig's `SuggestionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestionType {
    Folder,
    File,
    Arg,
    Subcommand,
    Option,
    Special,
    Mixin,
    Shortcut,
}

/// One of Fig's prebuilt generators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Template {
    FilePaths,
    Folders,
    History,
    Help,
}

/// A generator: a script to run plus how to read what it prints. Fig turned the
/// output into suggestions with JavaScript; see `generators.ron` for the
/// declarative rules that stand in for it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneratorSpec {
    pub script: Vec<String>,
    /// Fig built the argv from the typed tokens, which needs JavaScript.
    pub has_dynamic_script: bool,
    pub cwd: Option<String>,
    pub script_timeout_ms: Option<u64>,
    /// Fig's `splitOn`: cut the output on this string, one suggestion per part.
    pub split_on: Option<String>,
    pub template: Vec<Template>,
    pub has_filter_template_suggestions: bool,
    pub has_post_process: bool,
    pub has_custom: bool,
    /// When to throw away cached suggestions and generate again.
    pub trigger: Option<Trigger>,
    pub has_dynamic_trigger: bool,
    /// Which part of the typed token filters the suggestions.
    pub query_term: Option<QueryTerm>,
    pub has_dynamic_query_term: bool,
    pub cache: Option<CacheSpec>,
}

/// Fig's `trigger`, in the forms that do not need JavaScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    /// Regenerate on every keystroke.
    OnChange,
    /// Regenerate once the token reaches this length.
    OnThreshold(u32),
    /// Regenerate when the count of one of these strings changes.
    OnMatch(Vec<String>),
}

/// Fig's `getQueryTerm` string form: filter on the text after the last
/// occurrence of this separator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryTerm {
    AfterLast(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSpec {
    pub ttl_ms: Option<u64>,
    pub by_directory: bool,
    /// Serve stale suggestions while refreshing, rather than waiting.
    pub stale_while_revalidate: bool,
}

/// How the parser should read this command's tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ParserDirectives {
    /// `-work` is one flag, not `-w -o -r -k`.
    pub flags_are_posix_noncompliant: bool,
    pub options_must_precede_arguments: bool,
    pub option_arg_separators: Vec<String>,
}

/// How a suggestion is shown and what it inserts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Presentation {
    pub priority: Option<f32>,
    pub hidden: bool,
    pub icon: Option<String>,
    pub display_name: Option<String>,
    pub insert_value: Option<String>,
    pub dangerous: bool,
}

/// How a typed token is matched against suggestions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterStrategy {
    #[default]
    Default,
    Fuzzy,
    Prefix,
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
    fn the_snapshot_is_complete_and_every_document_parses() {
        let store = SpecStore::builtins();
        assert_eq!(store.len(), 1_484);
        assert!(store.get("az").is_some());
        assert!(store.get_by_id("az/2.53.0").is_some());
        // Parsing the whole snapshot is what catches a malformed document: a
        // spec nobody completes today would otherwise panic in front of a user.
        for &(id, ..) in SPEC_INDEX {
            assert!(
                store.get_by_id(id).is_some(),
                "{id} must deserialize into a spec"
            );
        }
    }
}
