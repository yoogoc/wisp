//! Fig-style completion generators, without a JavaScript runtime.
//!
//! A Fig generator runs a shell `script` and turns its stdout into suggestions
//! with a `postProcess` or `custom` JavaScript callback. Wisp imports the script
//! argv as data but cannot run the callback, so every imported generator would
//! otherwise be inert. This module replaces the callback with a declarative
//! [`Pipeline`]: rules in `generators.ron` name the shape of a command's output,
//! and anything unmatched falls back to [`Pipeline::Auto`], which recognises
//! JSON, newline-delimited JSON, and plain lines on its own.
//!
//! Scripts only run when their program is on the `allowed` list in
//! `generators.ron`, so an imported spec still cannot execute an arbitrary
//! command.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde::Deserialize;

use crate::{GeneratorSpec, Native, Trigger};
use serde_json::Value;
use tokio::{io::AsyncReadExt, process::Command, time::timeout};
use tracing::debug;
use wisp_config::GeneratorConfig;

/// One completion candidate produced by a generator.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub name: String,
    pub description: Option<String>,
    /// What to insert when the text shown is not the text wanted -- a stash
    /// message labels the entry, but `stash@{0}` is what the shell needs.
    pub insert: Option<String>,
}

/// What a generator's suggestions represent, so the UI can pick an icon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum SuggestionKind {
    #[default]
    Value,
    Branch,
    File,
    Directory,
}

#[derive(Debug, Deserialize)]
pub struct GeneratorRules {
    /// Programs an imported generator may spawn. Everything else is refused.
    allowed: HashSet<String>,
    /// Matched against the head of an imported argv; the longest match wins.
    #[serde(default)]
    rules: Vec<GeneratorRule>,
    /// What to read when a command's generator is JavaScript-only, keyed by the
    /// command path it sits under, such as `npm run`.
    #[serde(default)]
    natives: HashMap<String, Native>,
    /// A tool that reports a problem on stdout and still exits zero -- `git`
    /// answering `fatal: not a git repository` -- has produced no suggestions.
    #[serde(default)]
    reject_prefixes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorRule {
    /// The argv to run. A rule in `rules` also matches on it, by prefix.
    script: Vec<String>,
    #[serde(default)]
    pipeline: Pipeline,
    #[serde(default)]
    kind: SuggestionKind,
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Output prefixes that mean this generator has nothing to say, on top of
    /// the ones every generator rejects.
    #[serde(default)]
    reject_prefixes: Vec<String>,
}

/// How to turn a script's stdout into suggestions -- the declarative stand-in
/// for Fig's `postProcess` and `splitOn`.
#[derive(Debug, Clone, Default, Deserialize)]
pub enum Pipeline {
    /// Recognise the output's shape: JSON, newline-delimited JSON, or lines.
    #[default]
    Auto,
    Lines(LinesPipeline),
    Json(JsonPipeline),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LinesPipeline {
    separator: String,
    /// Header lines to drop before reading suggestions.
    skip: usize,
    /// Marker stripped from the front of a line, such as git's `* ` on the
    /// checked-out branch.
    strip_prefix: Option<String>,
    /// Splits a line into columns. Without it the whole line is the name.
    delimiter: Option<String>,
    name: usize,
    description: Option<Column>,
    /// The column to insert, when it differs from the one displayed.
    insert: Option<Column>,
    /// Lines holding any of these are not suggestions -- a section header in
    /// `pnpm ls`, a setting in `brew list`.
    reject_containing: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum Column {
    /// A single column.
    At(usize),
    /// Everything from this column to the end of the line.
    From(usize),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JsonPipeline {
    /// Object keys to walk before reading suggestions, e.g. `["Buckets"]`.
    path: Vec<String>,
    /// Field holding the suggestion text. Without it the name is guessed.
    name: Option<String>,
    description: Option<String>,
    /// The field to insert, when it differs from the one displayed.
    insert: Option<String>,
    /// Suggest the keys of the object at `path` rather than its values -- how
    /// `cargo read-manifest` lists a crate's features.
    keys: bool,
}

/// Object keys that hold a suggestion's text, best first.
const NAME_KEYS: &[&str] = &[
    "name", "Name", "Names", "id", "Id", "ID", "key", "Key", "alias", "slug", "title", "value",
];

/// Object keys that describe a suggestion, best first.
const DESCRIPTION_KEYS: &[&str] = &[
    "description",
    "Description",
    "desc",
    "summary",
    "status",
    "Status",
    "state",
    "State",
    "title",
];

#[derive(Debug)]
struct CacheEntry {
    fetched: Instant,
    suggestions: Arc<[Suggestion]>,
}

/// Runs generators and caches their output per working directory.
#[derive(Debug)]
pub struct GeneratorRuntime {
    rules: GeneratorRules,
    config: GeneratorConfig,
    cache: Mutex<HashMap<(Vec<String>, PathBuf), CacheEntry>>,
}

static RULES: &str = include_str!("generators.ron");

impl GeneratorRuntime {
    pub fn new(config: GeneratorConfig) -> Arc<Self> {
        let rules: GeneratorRules =
            ron::from_str(RULES).expect("built-in generator rules must be valid RON");
        Arc::new(Self {
            rules,
            config,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The process-wide runtime, so every engine shares one cache.
    pub fn shared() -> &'static Arc<Self> {
        static SHARED: OnceLock<Arc<GeneratorRuntime>> = OnceLock::new();
        SHARED.get_or_init(|| GeneratorRuntime::new(GeneratorConfig::default()))
    }

    /// Runs a generator imported from a Fig spec. Fig would have handed the
    /// output to a JavaScript callback; a rule from `generators.ron` reads it
    /// instead, falling back to the generator's own `splitOn` and then to
    /// detecting the output's shape.
    pub async fn run(&self, generator: &GeneratorSpec, token: &str, cwd: &Path) -> Vec<Suggestion> {
        // Fig regenerates on a threshold when the answer is only meaningful
        // once the user has typed enough to narrow it down.
        if let Some(Trigger::OnThreshold(length)) = generator.trigger
            && token.chars().count() < length as usize
        {
            return Vec::new();
        }
        if generator.script.is_empty() {
            // A dynamic script, a `custom` callback, or a template-only
            // generator: nothing here can run without JavaScript.
            return Vec::new();
        }
        let cwd = generator
            .cwd
            .as_deref()
            .map_or_else(|| cwd.to_path_buf(), PathBuf::from);
        let rule = self.matching_rule(&generator.script);
        let script = rule.map_or(generator.script.as_slice(), |rule| rule.script.as_slice());

        let cache = generator.cache.as_ref();
        let ttl = if generator.trigger == Some(Trigger::OnChange) {
            // Fig regenerates on every keystroke, so nothing may be reused.
            Duration::ZERO
        } else {
            Duration::from_millis(
                cache
                    .and_then(|cache| cache.ttl_ms)
                    .unwrap_or(self.config.cache_ttl_ms),
            )
        };
        // Fig caches globally unless a generator asks for per-directory caching;
        // a script that reads the working directory is the common case, so keep
        // the directory in the key unless the spec says the result is global.
        let key = (script.to_vec(), cwd.clone());
        if let Some(cached) = self.cached(&key, ttl) {
            return cached.to_vec();
        }

        let requested_timeout_ms = generator
            .script_timeout_ms
            .or_else(|| rule.and_then(|rule| rule.timeout_ms))
            .unwrap_or(self.config.timeout_ms);
        let timeout_ms = requested_timeout_ms.min(self.config.timeout_ms);
        let pipeline = match rule {
            Some(rule) => rule.pipeline.clone(),
            None => match &generator.split_on {
                Some(separator) => Pipeline::Lines(LinesPipeline {
                    separator: separator.clone(),
                    ..LinesPipeline::default()
                }),
                None => Pipeline::Auto,
            },
        };

        let Some(output) = self.execute(script, &cwd, timeout_ms).await else {
            return Vec::new();
        };
        let trimmed = output.trim_start();
        let rejected = self
            .rules
            .reject_prefixes
            .iter()
            .chain(rule.into_iter().flat_map(|rule| &rule.reject_prefixes))
            .any(|prefix| trimmed.starts_with(prefix.as_str()));
        if rejected {
            debug!(program = %script[0], "generator reported a problem on stdout");
            return Vec::new();
        }
        let mut suggestions = pipeline.apply(&output);
        let mut seen = HashSet::new();
        suggestions.retain(|suggestion| seen.insert(suggestion.name.clone()));
        self.store(key, &suggestions);
        suggestions
    }

    /// What Wisp reads instead of running the JavaScript Fig would have run.
    pub fn native(&self, path: &str) -> Option<Native> {
        self.rules.natives.get(path).copied()
    }

    pub fn history_limit(&self) -> usize {
        self.config.history_limit
    }

    /// What kind of value this generator yields, for icon selection.
    pub fn kind_of(&self, generator: &GeneratorSpec) -> SuggestionKind {
        self.matching_rule(&generator.script)
            .map_or(SuggestionKind::Value, |rule| rule.kind)
    }

    /// The longest rule whose script is a prefix of `script`.
    ///
    /// Importing an argument that carried several Fig generators concatenates
    /// their argv, so a rule runs its own script rather than what it matched.
    fn matching_rule(&self, script: &[String]) -> Option<&GeneratorRule> {
        self.rules
            .rules
            .iter()
            .filter(|rule| script.starts_with(&rule.script))
            .max_by_key(|rule| rule.script.len())
    }

    async fn execute(&self, script: &[String], cwd: &Path, timeout_ms: u64) -> Option<String> {
        let program = script.first()?;
        if !self.rules.allowed.contains(program) {
            debug!(program, "generator program is not allowed to run");
            return None;
        }
        let mut command = Command::new(program);
        command
            .args(&script[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().ok()?;
        let stdout = child.stdout.take()?;
        let limit = u64::try_from(self.config.max_output_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let operation = async move {
            let mut bytes = Vec::new();
            stdout.take(limit).read_to_end(&mut bytes).await.ok()?;
            if bytes.len() > self.config.max_output_bytes {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return None;
            }
            child.wait().await.ok()?.success().then_some(bytes)
        };
        let Ok(Some(output)) = timeout(Duration::from_millis(timeout_ms), operation).await else {
            debug!(program, "generator timed out or failed to spawn");
            return None;
        };
        Some(String::from_utf8_lossy(&output).into_owned())
    }

    fn cached(&self, key: &(Vec<String>, PathBuf), ttl: Duration) -> Option<Arc<[Suggestion]>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;
        (entry.fetched.elapsed() < ttl).then(|| Arc::clone(&entry.suggestions))
    }

    fn store(&self, key: (Vec<String>, PathBuf), suggestions: &[Suggestion]) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                key,
                CacheEntry {
                    fetched: Instant::now(),
                    suggestions: suggestions.into(),
                },
            );
        }
    }
}

impl Pipeline {
    fn apply(&self, output: &str) -> Vec<Suggestion> {
        match self {
            Self::Auto => auto(output),
            Self::Lines(lines) => lines.apply(output),
            Self::Json(json) => documents(output)
                .iter()
                .flat_map(|value| json.apply(value))
                .collect(),
        }
    }
}

impl LinesPipeline {
    fn apply(&self, output: &str) -> Vec<Suggestion> {
        let separator = if self.separator.is_empty() {
            "\n"
        } else {
            self.separator.as_str()
        };
        output
            .split(separator)
            .skip(self.skip)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| {
                !self
                    .reject_containing
                    .iter()
                    .any(|marker| line.contains(marker.as_str()))
            })
            .filter_map(|line| self.suggestion(line))
            .collect()
    }

    fn suggestion(&self, line: &str) -> Option<Suggestion> {
        let line = match &self.strip_prefix {
            Some(prefix) => line.strip_prefix(prefix.as_str()).unwrap_or(line).trim(),
            None => line,
        };
        let Some(delimiter) = &self.delimiter else {
            return (!line.is_empty()).then(|| Suggestion {
                name: line.to_owned(),
                description: None,
                insert: None,
            });
        };
        let columns: Vec<&str> = if delimiter.is_empty() {
            line.split_whitespace().collect()
        } else {
            line.split(delimiter.as_str()).map(str::trim).collect()
        };
        let name = columns.get(self.name)?;
        if name.is_empty() {
            return None;
        }
        let column = |column: Column| match column {
            Column::At(index) => columns.get(index).map(|value| (*value).to_owned()),
            Column::From(index) => columns
                .get(index..)
                .filter(|rest| !rest.is_empty())
                .map(|rest| rest.join(" ")),
        };
        Some(Suggestion {
            name: (*name).to_owned(),
            description: self
                .description
                .and_then(column)
                .filter(|value| !value.is_empty()),
            insert: self
                .insert
                .and_then(column)
                .filter(|value| !value.is_empty()),
        })
    }
}

impl JsonPipeline {
    fn apply(&self, value: &Value) -> Vec<Suggestion> {
        let mut current = value;
        for key in &self.path {
            match current.get(key) {
                Some(next) => current = next,
                None => return Vec::new(),
            }
        }
        if self.keys {
            return current
                .as_object()
                .map(|object| {
                    object
                        .keys()
                        .map(|key| Suggestion {
                            name: key.clone(),
                            description: None,
                            insert: None,
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        let mut suggestions = Vec::new();
        collect_json(current, self, &mut suggestions);
        suggestions
    }
}

/// Reads suggestions out of a JSON value, descending through arrays and into
/// the first array an object holds when that object is not a suggestion itself.
fn collect_json(value: &Value, fields: &JsonPipeline, suggestions: &mut Vec<Suggestion>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_json(item, fields, suggestions);
            }
        }
        Value::Object(_) => {
            if let Some(suggestion) = object_suggestion(value, fields) {
                suggestions.push(suggestion);
                return;
            }
            if let Some(nested) = value.as_object().and_then(|object| {
                object
                    .values()
                    .find(|nested| matches!(nested, Value::Array(_)))
            }) {
                collect_json(nested, fields, suggestions);
            }
        }
        Value::String(text) => suggestions.push(Suggestion {
            name: text.clone(),
            description: None,
            insert: None,
        }),
        Value::Number(_) | Value::Bool(_) => suggestions.push(Suggestion {
            name: value.to_string(),
            description: None,
            insert: None,
        }),
        Value::Null => {}
    }
}

fn object_suggestion(value: &Value, fields: &JsonPipeline) -> Option<Suggestion> {
    let name = match &fields.name {
        Some(key) => scalar(value.get(key)?)?,
        None => NAME_KEYS
            .iter()
            .find_map(|key| value.get(key).and_then(scalar))?,
    };
    let description = match &fields.description {
        Some(key) => value.get(key).and_then(scalar),
        None => DESCRIPTION_KEYS
            .iter()
            .find_map(|key| value.get(key).and_then(scalar)),
    };
    Some(Suggestion {
        name,
        description: description.filter(|value| !value.is_empty()),
        insert: fields
            .insert
            .as_ref()
            .and_then(|key| value.get(key))
            .and_then(scalar),
    })
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

/// Recognises the three output shapes a generator's script tends to produce.
fn auto(output: &str) -> Vec<Suggestion> {
    let fields = JsonPipeline::default();
    let mut suggestions = Vec::new();
    for value in documents(output) {
        collect_json(&value, &fields, &mut suggestions);
    }
    if !suggestions.is_empty() {
        return suggestions;
    }
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| Suggestion {
            name: line.to_owned(),
            description: None,
            insert: None,
        })
        .collect()
}

/// Parses the output as one JSON document, or as newline-delimited JSON --
/// `docker ps --format '{{ json . }}'` and friends print one object per line.
fn documents(output: &str) -> Vec<Value> {
    let trimmed = output.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return Vec::new();
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return vec![value];
    }
    let mut values = Vec::new();
    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        values.push(value);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline(source: &str) -> Pipeline {
        ron::from_str(source).expect("test pipeline must be valid RON")
    }

    fn names(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions
            .iter()
            .map(|suggestion| suggestion.name.as_str())
            .collect()
    }

    fn script(words: &[&str]) -> GeneratorSpec {
        GeneratorSpec {
            script: words.iter().map(|word| (*word).to_owned()).collect(),
            ..GeneratorSpec::default()
        }
    }

    #[test]
    fn built_in_rules_are_valid() {
        let runtime = GeneratorRuntime::shared();
        assert!(!runtime.rules.rules.is_empty());
        assert!(runtime.rules.allowed.contains("git"));
    }

    #[test]
    fn the_longest_matching_rule_wins() {
        let runtime = GeneratorRuntime::shared();
        let generator = script(&[
            "git",
            "--no-optional-locks",
            "branch",
            "--no-color",
            "--sort=-committerdate",
        ]);
        assert_eq!(runtime.kind_of(&generator), SuggestionKind::Branch);

        // A rule matches on the head of a longer argv and still runs its own
        // script rather than the extra words.
        let mut longer = generator.script.clone();
        longer.push("--verbose".to_owned());
        let rule = runtime
            .matching_rule(&longer)
            .expect("the head of a longer argv still matches");
        assert_eq!(rule.script, generator.script);
    }

    #[test]
    fn lines_strip_the_checked_out_branch_marker() {
        let suggestions = pipeline(r#"Lines((strip_prefix: Some("* ")))"#)
            .apply("* main\n  release/1.0\n\n  work\n");
        assert_eq!(names(&suggestions), ["main", "release/1.0", "work"]);
    }

    #[test]
    fn columns_split_a_name_from_its_description() {
        let suggestions =
            pipeline(r#"Lines((delimiter: Some(""), name: 0, description: Some(From(1))))"#)
                .apply("9fceb02 Fix the parser\n1a2b3c4 Add a test\n");
        assert_eq!(names(&suggestions), ["9fceb02", "1a2b3c4"]);
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Fix the parser")
        );
    }

    #[test]
    fn auto_descends_into_the_array_an_object_wraps() {
        let suggestions = auto(r#"{"Buckets":[{"Name":"logs"},{"Name":"assets"}],"Owner":{}}"#);
        assert_eq!(names(&suggestions), ["logs", "assets"]);
    }

    #[test]
    fn auto_reads_newline_delimited_json() {
        let suggestions =
            auto("{\"ID\":\"a1\",\"Names\":\"web\"}\n{\"ID\":\"b2\",\"Names\":\"db\"}\n");
        assert_eq!(names(&suggestions), ["web", "db"]);
    }

    #[test]
    fn auto_falls_back_to_plain_lines() {
        let suggestions = auto("origin\nupstream\n");
        assert_eq!(names(&suggestions), ["origin", "upstream"]);
    }

    #[test]
    fn a_named_json_field_overrides_the_guess() {
        let suggestions = pipeline(r#"Json((name: Some("number"), description: Some("title")))"#)
            .apply(r#"[{"number":12,"title":"Fix the parser","state":"OPEN"}]"#);
        assert_eq!(names(&suggestions), ["12"]);
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Fix the parser")
        );
    }

    #[tokio::test]
    async fn a_program_outside_the_allow_list_never_runs() {
        let suggestions = GeneratorRuntime::shared()
            .run(
                &script(&["sh", "-c", "echo pwned"]),
                "",
                Path::new(env!("CARGO_MANIFEST_DIR")),
            )
            .await;
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn a_generator_without_a_script_yields_nothing() {
        let generator = GeneratorSpec {
            has_custom: true,
            ..GeneratorSpec::default()
        };
        let suggestions = GeneratorRuntime::shared()
            .run(&generator, "", Path::new(env!("CARGO_MANIFEST_DIR")))
            .await;
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn a_generator_lists_this_repository_branches() {
        let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(status) = std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(cwd)
            .output()
        else {
            return;
        };
        if !status.status.success() {
            return;
        }
        let suggestions = GeneratorRuntime::shared()
            .run(
                &script(&[
                    "git",
                    "--no-optional-locks",
                    "branch",
                    "--no-color",
                    "--sort=-committerdate",
                ]),
                "",
                cwd,
            )
            .await;
        assert!(!suggestions.is_empty());
    }
}
