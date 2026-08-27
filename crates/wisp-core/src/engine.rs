use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::{process::Command, time::timeout};
use tracing::debug;
use wisp_protocol::{BufferSnapshot, Candidate, CandidateKind};

use crate::{
    ArgumentSpec, CommandSpec, CompletionContext, OptionSpec, SpecStore, SubcommandSpec,
    SuggestionSource, parse_completion_context,
};

#[derive(Debug, Clone)]
pub struct CompletionEngine {
    specs: SpecStore,
    commands: Arc<[String]>,
    max_candidates: usize,
}

impl Default for CompletionEngine {
    fn default() -> Self {
        Self {
            specs: SpecStore::builtins(),
            commands: discover_commands().into(),
            max_candidates: 0,
        }
    }
}

impl CompletionEngine {
    pub fn new(specs: SpecStore) -> Self {
        Self {
            specs,
            commands: discover_commands().into(),
            max_candidates: 0,
        }
    }

    /// Set the number of candidates retained after ranking. Zero is unlimited.
    pub fn with_max_candidates(mut self, max_candidates: usize) -> Self {
        self.max_candidates = max_candidates;
        self
    }

    pub async fn complete(&self, snapshot: &BufferSnapshot) -> Vec<Candidate> {
        if snapshot.buffer.trim().is_empty() {
            return Vec::new();
        }
        let context = parse_completion_context(&snapshot.buffer, snapshot.cursor);
        let mut candidates = if is_command_position(&context) {
            complete_commands(&context, &self.commands)
        } else {
            self.complete_arguments(&context, &snapshot.cwd).await
        };

        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.label.cmp(&right.label))
        });
        candidates.dedup_by(|left, right| left.insert_text == right.insert_text);
        if self.max_candidates > 0 {
            candidates.truncate(self.max_candidates);
        }
        candidates
    }

    async fn complete_arguments(&self, context: &CompletionContext, cwd: &Path) -> Vec<Candidate> {
        let Some(command) = context.command.as_deref() else {
            return complete_commands(context, &self.commands);
        };
        let Some(spec) = self.specs.get(command) else {
            return complete_paths(context, cwd, false);
        };

        let resolved = self.resolve_node(spec, &context.args);

        if context.current_token.starts_with('-') {
            return resolved
                .node
                .options()
                .iter()
                .flat_map(|option| {
                    option.names.iter().filter_map(|name| {
                        make_candidate(
                            name,
                            name,
                            option.description.clone(),
                            CandidateKind::Option,
                            context,
                        )
                    })
                })
                .collect();
        }

        if let Some(argument) = resolved.pending_argument {
            return self.complete_argument(argument, context, cwd).await;
        }

        let mut candidates: Vec<Candidate> = resolved
            .node
            .subcommands()
            .iter()
            .flat_map(|subcommand| {
                std::iter::once(&subcommand.name)
                    .chain(subcommand.aliases.iter())
                    .filter_map(|name| {
                        make_candidate(
                            name,
                            name,
                            subcommand.description.clone(),
                            CandidateKind::Subcommand,
                            context,
                        )
                    })
            })
            .chain(resolved.node.options().iter().flat_map(|option| {
                option.names.iter().filter_map(|name| {
                    make_candidate(
                        name,
                        name,
                        option.description.clone(),
                        CandidateKind::Option,
                        context,
                    )
                })
            }))
            .collect();

        if let Some(argument) = argument_at(resolved.node.arguments(), resolved.positional_index) {
            candidates.extend(self.complete_argument(argument, context, cwd).await);
        } else if candidates.is_empty() {
            candidates.extend(complete_paths(context, cwd, false));
        }
        candidates
    }

    fn resolve_node<'a>(&'a self, spec: &'a CommandSpec, args: &'a [String]) -> ResolvedNode<'a> {
        let mut node = NodeRef::Root(spec);
        let mut positional_index = 0;
        let mut pending_argument = None;

        for token in args {
            if pending_argument.take().is_some() {
                continue;
            }

            if token.starts_with('-') {
                if let Some(option) = find_option(node.options(), token) {
                    pending_argument = option.arguments.first();
                    if pending_argument.is_none() && option.takes_value {
                        // A legacy takes-value option consumes one token without suggestions.
                        pending_argument = Some(&UNAVAILABLE_ARGUMENT);
                    }
                }
                continue;
            }

            if let Some(subcommand) = node
                .subcommands()
                .iter()
                .find(|subcommand| subcommand.matches(token))
            {
                node = NodeRef::Subcommand(subcommand);
                positional_index = 0;
                if let Some(load_spec) = &subcommand.load_spec
                    && let Some(loaded) = self.specs.get_by_id(load_spec)
                {
                    node = NodeRef::Root(loaded);
                }
                continue;
            }
            positional_index += 1;
        }

        ResolvedNode {
            node,
            positional_index,
            pending_argument,
        }
    }

    async fn complete_argument(
        &self,
        argument: &ArgumentSpec,
        context: &CompletionContext,
        cwd: &Path,
    ) -> Vec<Candidate> {
        match &argument.source {
            SuggestionSource::Files => complete_paths(context, cwd, false),
            SuggestionSource::Directories => complete_paths(context, cwd, true),
            SuggestionSource::Static(values) => values
                .iter()
                .filter_map(|value| {
                    make_candidate(
                        value,
                        value,
                        Some(argument.name.clone()),
                        CandidateKind::Subcommand,
                        context,
                    )
                })
                .collect(),
            SuggestionSource::Generator(generator) => {
                self.run_generator(generator, context, cwd).await
            }
            SuggestionSource::Unavailable => Vec::new(),
        }
    }

    async fn run_generator(
        &self,
        generator: &str,
        context: &CompletionContext,
        cwd: &Path,
    ) -> Vec<Candidate> {
        let (program, args, kind) = match generator {
            "git.branches" => (
                "git",
                vec![
                    "for-each-ref",
                    "--format=%(refname:short)",
                    "refs/heads",
                    "refs/remotes",
                ],
                CandidateKind::Branch,
            ),
            "docker.containers" => (
                "docker",
                vec!["ps", "--format", "{{.Names}}"],
                CandidateKind::Subcommand,
            ),
            _ => return Vec::new(),
        };

        let output = timeout(
            Duration::from_millis(150),
            Command::new(program).args(args).current_dir(cwd).output(),
        )
        .await;
        let Ok(Ok(output)) = output else {
            debug!(generator, "completion generator timed out or failed");
            return Vec::new();
        };
        if !output.status.success() || output.stdout.len() > 256 * 1024 {
            return Vec::new();
        }

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| make_candidate(line, line, None, kind, context))
            .collect()
    }
}

static UNAVAILABLE_ARGUMENT: ArgumentSpec = ArgumentSpec {
    name: String::new(),
    source: SuggestionSource::Unavailable,
    optional: false,
    variadic: false,
    imported_generator: None,
};

#[derive(Clone, Copy)]
enum NodeRef<'a> {
    Root(&'a CommandSpec),
    Subcommand(&'a SubcommandSpec),
}

impl<'a> NodeRef<'a> {
    fn subcommands(self) -> &'a [SubcommandSpec] {
        match self {
            Self::Root(spec) => &spec.subcommands,
            Self::Subcommand(spec) => &spec.subcommands,
        }
    }

    fn options(self) -> &'a [OptionSpec] {
        match self {
            Self::Root(spec) => &spec.options,
            Self::Subcommand(spec) => &spec.options,
        }
    }

    fn arguments(self) -> &'a [ArgumentSpec] {
        match self {
            Self::Root(spec) => &spec.arguments,
            Self::Subcommand(spec) => &spec.arguments,
        }
    }
}

struct ResolvedNode<'a> {
    node: NodeRef<'a>,
    positional_index: usize,
    pending_argument: Option<&'a ArgumentSpec>,
}

fn find_option<'a>(options: &'a [OptionSpec], token: &str) -> Option<&'a OptionSpec> {
    let name = token.split_once('=').map_or(token, |(name, _)| name);
    options
        .iter()
        .find(|option| option.names.iter().any(|candidate| candidate == name))
}

fn argument_at(arguments: &[ArgumentSpec], index: usize) -> Option<&ArgumentSpec> {
    arguments
        .get(index)
        .or_else(|| arguments.last().filter(|argument| argument.variadic))
}

fn is_command_position(context: &CompletionContext) -> bool {
    context.args.is_empty()
        && context
            .command
            .as_deref()
            .is_some_and(|command| command == context.current_token)
}

fn discover_commands() -> Vec<String> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut commands = Vec::new();
    for directory in std::env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_executable(&path) {
                continue;
            }
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            if seen.insert(name.to_owned()) {
                commands.push(name.to_owned());
            }
        }
    }
    commands.sort();
    commands
}

fn complete_commands(context: &CompletionContext, commands: &[String]) -> Vec<Candidate> {
    commands
        .iter()
        .filter_map(|name| {
            make_candidate(
                name,
                name,
                Some("command".into()),
                CandidateKind::Command,
                context,
            )
        })
        .collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn complete_paths(
    context: &CompletionContext,
    cwd: &Path,
    directories_only: bool,
) -> Vec<Candidate> {
    let token = &context.current_token;
    let token_path = Path::new(token);
    let (search_directory, display_parent, prefix) = if token.ends_with('/') {
        (cwd.join(token_path), token_path.to_path_buf(), "")
    } else {
        let (search_directory, display_parent) = match token_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                (cwd.join(parent), parent.to_path_buf())
            }
            _ => (cwd.to_path_buf(), PathBuf::new()),
        };
        let prefix = token_path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        (search_directory, display_parent, prefix)
    };
    let Ok(entries) = std::fs::read_dir(search_directory) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if directories_only && !file_type.is_dir() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            if !prefix.is_empty() && name.starts_with('.') != prefix.starts_with('.') {
                return None;
            }
            let relative = display_parent.join(&name);
            let mut insert = relative.to_string_lossy().into_owned();
            if file_type.is_dir() {
                insert.push('/');
            }
            let kind = if file_type.is_dir() {
                CandidateKind::Directory
            } else {
                CandidateKind::File
            };
            make_candidate(&insert, &shell_escape(&insert), None, kind, context)
        })
        .collect()
}

fn make_candidate(
    label: &str,
    insert_text: &str,
    description: Option<String>,
    kind: CandidateKind,
    context: &CompletionContext,
) -> Option<Candidate> {
    let score = match_score(&context.current_token, label)?;
    Some(Candidate {
        label: label.to_owned(),
        insert_text: insert_text.to_owned(),
        description,
        kind,
        score,
        replace_start: context.replace_range.start,
        replace_end: context.replace_range.end,
    })
}

fn match_score(query: &str, candidate: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.5);
    }
    let query_lower = query.to_lowercase();
    let candidate_lower = candidate.to_lowercase();
    if candidate_lower == query_lower {
        return Some(1.0);
    }
    if candidate_lower.starts_with(&query_lower) {
        return Some(0.9 - (candidate.len().saturating_sub(query.len()) as f64 * 0.001));
    }
    let mut position = 0;
    let mut gaps = 0usize;
    for needle in query_lower.chars() {
        let offset = candidate_lower[position..].find(needle)?;
        gaps += offset;
        position += offset + needle.len_utf8();
    }
    Some((0.7 - gaps as f64 * 0.01).max(0.1))
}

fn shell_escape(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wisp_protocol::{ShellKind, TerminalKind, TerminalSnapshot};

    use super::*;

    fn snapshot(buffer: &str) -> BufferSnapshot {
        BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: buffer.into(),
            cursor: buffer.chars().count(),
            cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 80,
                rows: 24,
                prompt: "$ ".into(),
                cursor_row: None,
                cursor_column: None,
                rendered: None,
                viewport: None,
            },
        }
    }

    #[tokio::test]
    async fn completes_git_subcommands() {
        let values = CompletionEngine::default()
            .complete(&snapshot("git che"))
            .await;
        assert_eq!(
            values.first().map(|value| value.label.as_str()),
            Some("checkout")
        );
    }

    #[tokio::test]
    async fn completes_cargo_options() {
        let values = CompletionEngine::default()
            .complete(&snapshot("cargo build --r"))
            .await;
        assert!(values.iter().any(|value| value.label == "--release"));
    }

    #[tokio::test]
    async fn empty_prompt_does_not_take_over_history_navigation() {
        let values = CompletionEngine::default().complete(&snapshot("")).await;
        assert!(values.is_empty());
    }

    #[tokio::test]
    async fn trailing_directory_separator_completes_children() {
        let values = CompletionEngine::default()
            .complete(&snapshot("cat src/"))
            .await;
        assert!(values.iter().any(|value| value.label == "src/lib.rs"));
    }

    #[tokio::test]
    async fn completes_deep_fig_subcommands() {
        let values = CompletionEngine::default()
            .complete(&snapshot("docker compose up --d"))
            .await;
        assert!(values.iter().any(|value| value.label == "--detach"));
    }

    #[tokio::test]
    async fn completes_fig_static_argument_suggestions() {
        let values = CompletionEngine::default()
            .complete(&snapshot("git commit --cleanup "))
            .await;
        assert!(values.iter().any(|value| value.label == "strip"));
    }

    #[tokio::test]
    async fn keeps_more_than_one_overlay_page_of_candidates() {
        let values = CompletionEngine::default()
            .complete(&snapshot("git "))
            .await;
        assert!(values.len() > 12);
    }

    #[tokio::test]
    async fn configured_candidate_limit_is_applied() {
        let values = CompletionEngine::default()
            .with_max_candidates(5)
            .complete(&snapshot("git "))
            .await;
        assert_eq!(values.len(), 5);
    }
}
