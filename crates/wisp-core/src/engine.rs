use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

use wisp_config::GeneratorConfig;
use wisp_protocol::{BufferSnapshot, Candidate, CandidateKind};

use crate::ranking::{DEFAULT_PRIORITY, RankingStore};
use crate::{
    AliasResolverSpec, ArgumentSpec, CommandSpec, CompletionContext, FilterStrategy,
    GeneratorRuntime, GeneratorSpec, Native, OptionSpec, ParserDirectives, Presentation, QueryTerm,
    Repeat, SpecStore, SubcommandSpec, Suggestion, SuggestionKind, SuggestionSpec, SuggestionType,
    Template, parse_completion_context, parser::tokenize_words,
};

#[derive(Debug, Clone)]
pub struct CompletionEngine {
    specs: SpecStore,
    commands: Arc<[String]>,
    generators: Arc<GeneratorRuntime>,
    ranking: Arc<RankingStore>,
    max_candidates: usize,
}

impl Default for CompletionEngine {
    fn default() -> Self {
        Self {
            specs: SpecStore::builtins(),
            commands: discover_commands().into(),
            generators: Arc::clone(GeneratorRuntime::shared()),
            ranking: Arc::new(RankingStore::default()),
            max_candidates: 0,
        }
    }
}

impl CompletionEngine {
    pub fn new(specs: SpecStore) -> Self {
        Self {
            specs,
            commands: discover_commands().into(),
            generators: Arc::clone(GeneratorRuntime::shared()),
            ranking: Arc::new(RankingStore::default()),
            max_candidates: 0,
        }
    }

    /// Set the number of candidates retained after ranking. Zero is unlimited.
    pub fn with_max_candidates(mut self, max_candidates: usize) -> Self {
        self.max_candidates = max_candidates;
        self
    }

    pub fn with_generator_config(mut self, config: GeneratorConfig) -> Self {
        self.generators = GeneratorRuntime::new(config);
        self
    }

    pub fn with_recency_ranking(mut self, enabled: bool, path: PathBuf) -> Self {
        self.ranking = Arc::new(RankingStore::load(enabled, path));
        self
    }

    pub fn record_selection(
        &self,
        snapshot: &BufferSnapshot,
        candidate: &Candidate,
    ) -> std::io::Result<()> {
        let context = parse_completion_context(&snapshot.buffer, snapshot.cursor);
        self.ranking.record(
            context.command.as_deref().unwrap_or_default(),
            &candidate.label,
        )
    }

    pub async fn complete(&self, snapshot: &BufferSnapshot) -> Vec<Candidate> {
        if snapshot.buffer.trim().is_empty() {
            return Vec::new();
        }
        let context = parse_completion_context(&snapshot.buffer, snapshot.cursor);
        let mut candidates = if is_command_position(&context) {
            complete_commands(&context, &self.commands, &snapshot.cwd)
        } else {
            self.complete_arguments(&context, &snapshot.cwd).await
        };

        let command = context.command.as_deref().unwrap_or_default();
        for candidate in &mut candidates {
            candidate.priority =
                self.ranking
                    .priority(command, &candidate.label, candidate.priority);
        }

        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right.priority.total_cmp(&left.priority))
        });
        candidates.dedup_by(|left, right| left.insert_text == right.insert_text);
        if self.max_candidates > 0 {
            candidates.truncate(self.max_candidates);
        }
        candidates
    }

    async fn complete_arguments(&self, context: &CompletionContext, cwd: &Path) -> Vec<Candidate> {
        let Some(command) = context.command.as_deref() else {
            return complete_commands(context, &self.commands, cwd);
        };
        if !command_is_available(command, cwd) {
            return Vec::new();
        }
        let Some(spec) = self.specs.get(command) else {
            return complete_paths(context, cwd, false);
        };

        let expanded_args = self.expand_runtime_aliases(spec, &context.args, cwd).await;
        let resolved = self.resolve_node(spec, &expanded_args, false);
        let token = context.current_token.as_str();

        // `--message=<value>`: the option is already settled, so complete the
        // value after the separator and replace only that part of the token.
        if let Some((argument, offset)) = resolved.separated_argument(token) {
            let inner = CompletionContext {
                command: context.command.clone(),
                args: context.args.clone(),
                current_token: token[offset..].to_owned(),
                replace_range: (context.replace_range.start + offset)..context.replace_range.end,
            };
            return self
                .complete_argument(argument, &resolved, &inner, cwd)
                .await;
        }

        if token.starts_with('-') && token != "-" {
            return resolved.option_candidates(context);
        }

        if let Some(argument) = resolved.pending_argument {
            return self
                .complete_argument(argument, &resolved, context, cwd)
                .await;
        }

        let mut candidates: Vec<Candidate> = resolved
            .node
            .subcommands()
            .iter()
            .filter(|subcommand| !subcommand.presentation.hidden)
            .flat_map(|subcommand| {
                std::iter::once(&subcommand.name)
                    .chain(subcommand.aliases.iter())
                    .filter_map(|name| {
                        present(
                            name,
                            &subcommand.presentation,
                            subcommand.description.clone(),
                            CandidateKind::Subcommand,
                            context,
                        )
                    })
            })
            .collect();

        apply_filter_strategy(
            &mut candidates,
            resolved.node.filter_strategy(),
            &context.current_token,
        );

        let mut additional: Vec<Candidate> = resolved
            .node
            .additional_suggestions()
            .iter()
            .filter_map(|suggestion| {
                suggestion_candidate(suggestion, CandidateKind::Subcommand, context)
            })
            .collect();
        apply_filter_strategy(
            &mut additional,
            resolved.node.filter_strategy(),
            &context.current_token,
        );

        // A command that insists on a subcommand offers nothing else.
        if !resolved.node.requires_subcommand() {
            if let Some(argument) =
                argument_at(resolved.node.arguments(), resolved.positional_index)
            {
                candidates.extend(
                    self.complete_argument(argument, &resolved, context, cwd)
                        .await,
                );
            }
            candidates.extend(additional);
            if !resolved.options_are_closed() {
                candidates.extend(resolved.option_candidates(context));
            }
            if candidates.is_empty() {
                candidates.extend(complete_paths(context, cwd, false));
            }
        } else {
            candidates.extend(additional);
        }
        candidates
    }

    /// Expands tokens only when the argument that consumed them carries Fig's
    /// `parserDirectives.alias`. After every substitution the spec is walked
    /// again from the beginning, matching Fig's parser-state rollback.
    async fn expand_runtime_aliases(
        &self,
        spec: &CommandSpec,
        args: &[String],
        cwd: &Path,
    ) -> Vec<String> {
        let mut expanded = args.to_vec();
        let mut seen = HashSet::new();
        for _ in 0..8 {
            let resolved = self.resolve_node(spec, &expanded, true);
            let Some(request) = resolved.alias_request else {
                return expanded;
            };
            let index = request.token_index;
            let alias = expanded[index].clone();
            if !seen.insert((index, alias.clone())) {
                return expanded;
            }
            let Some(value) = self
                .generators
                .resolve_alias(request.resolver, &alias, cwd)
                .await
            else {
                return expanded;
            };
            let replacement = tokenize_words(&value);
            if replacement.is_empty()
                || replacement
                    .iter()
                    .any(|word| matches!(word.as_str(), "|" | "||" | "&&" | ";"))
            {
                return expanded;
            }
            expanded.splice(index..=index, replacement);
        }
        expanded
    }

    fn resolve_node<'a>(
        &'a self,
        spec: &'a CommandSpec,
        args: &'a [String],
        detect_alias: bool,
    ) -> ResolvedNode<'a> {
        self.resolve_node_at(spec, args, detect_alias, 0)
    }

    fn resolve_node_at<'a>(
        &'a self,
        spec: &'a CommandSpec,
        args: &'a [String],
        detect_alias: bool,
        token_offset: usize,
    ) -> ResolvedNode<'a> {
        let mut node = NodeRef::Root(spec);
        let mut parent = None;
        let mut positional_index = 0;
        let mut pending_argument = None;
        let mut persistent: Vec<&'a OptionSpec> = Vec::new();
        let mut used: Vec<&'a str> = Vec::new();
        let mut directives = node.parser_directives();
        let mut alias_request = None;

        for (index, token) in args.iter().enumerate() {
            if let Some(argument) = pending_argument.take() {
                if detect_alias && let Some(resolver) = argument_alias(argument) {
                    alias_request = Some(AliasRequest {
                        token_index: token_offset + index,
                        resolver,
                    });
                    break;
                }
                continue;
            }

            if is_option_token(token) && !variadic_swallows_options(node, positional_index) {
                let name =
                    split_separator(token, directives).map_or(token.as_str(), |(name, _)| name);
                used.push(name);
                if let Some(option) = find_option(node.options(), &persistent, name) {
                    // A separated option carries its value inside the token.
                    if split_separator(token, directives).is_none() {
                        pending_argument = option.arguments.first();
                    }
                }
                continue;
            }

            if let Some(subcommand) = node
                .subcommands()
                .iter()
                .find(|subcommand| subcommand.matches(token))
            {
                persistent.extend(node.options().iter().filter(|option| option.persistent));
                parent = Some(node);
                node = NodeRef::Subcommand(subcommand);
                if let Some(loaded) = self.load(subcommand.load_spec.as_deref()) {
                    node = NodeRef::Root(loaded);
                } else if let Some(inline) = &subcommand.load_spec_inline {
                    node = NodeRef::Subcommand(inline);
                }
                directives = node.parser_directives().or(directives);
                positional_index = 0;
                used.clear();
                continue;
            }

            // `sudo <command>` and friends: the rest of the line is a command
            // line of its own, so resolution restarts from that command's spec.
            if let Some(argument) = argument_at(node.arguments(), positional_index) {
                if detect_alias && let Some(resolver) = argument_alias(argument) {
                    alias_request = Some(AliasRequest {
                        token_index: token_offset + index,
                        resolver,
                    });
                    break;
                }
                if argument.is_command
                    && let Some(nested) = self.specs.get(token)
                {
                    return self.resolve_node_at(
                        nested,
                        &args[index + 1..],
                        detect_alias,
                        token_offset + index + 1,
                    );
                }
            }
            positional_index += 1;
        }

        ResolvedNode {
            node,
            parent,
            positional_index,
            pending_argument,
            persistent,
            used,
            directives,
            alias_request,
        }
    }

    fn load(&self, id: Option<&str>) -> Option<&CommandSpec> {
        id.and_then(|id| self.specs.get_by_id(id))
            .filter(|spec| !spec.placeholder)
    }

    /// Fig lets an argument carry fixed suggestions, prebuilt templates, and
    /// generators at once, and contributes whatever each of them yields.
    async fn complete_argument(
        &self,
        argument: &ArgumentSpec,
        resolved: &ResolvedNode<'_>,
        context: &CompletionContext,
        cwd: &Path,
    ) -> Vec<Candidate> {
        let mut candidates: Vec<Candidate> = argument
            .suggestions
            .iter()
            .filter_map(|suggestion| {
                suggestion_candidate(suggestion, CandidateKind::Subcommand, context)
            })
            .collect();

        for template in &argument.template {
            match template {
                Template::FilePaths => candidates.extend(complete_paths(context, cwd, false)),
                Template::Folders => candidates.extend(complete_paths(context, cwd, true)),
                Template::History => candidates.extend(candidates_from(
                    Native::History.suggest(cwd, self.generators.history_limit()),
                    SuggestionKind::Value,
                    context,
                )),
                // Fig's help template lists the siblings of the subcommand the
                // argument belongs to -- what `git help <command>` offers.
                Template::Help => {
                    if let Some(parent) = resolved.parent {
                        candidates.extend(parent.subcommands().iter().filter_map(|subcommand| {
                            present(
                                &subcommand.name,
                                &subcommand.presentation,
                                subcommand.description.clone(),
                                CandidateKind::Subcommand,
                                context,
                            )
                        }));
                    }
                }
            }
        }

        // An argument can load a whole spec, whose subcommands are its values.
        if let Some(loaded) = self.load(argument.load_spec.as_deref()) {
            candidates.extend(loaded.subcommands.iter().filter_map(|subcommand| {
                present(
                    &subcommand.name,
                    &subcommand.presentation,
                    subcommand.description.clone(),
                    CandidateKind::Subcommand,
                    context,
                )
            }));
        }

        let mut javascript_only = false;
        apply_filter_strategy(
            &mut candidates,
            argument.filter_strategy,
            &context.current_token,
        );

        for generator in &argument.generators {
            if generator.script.is_empty() {
                javascript_only = true;
                continue;
            }
            // Fig's `getQueryTerm` says which part of the token filters the
            // suggestions -- everything after the last `/`, say.
            let inner = query_context(generator, context);
            let inner = inner.as_ref().unwrap_or(context);
            let suggestions = self
                .generators
                .run(generator, &inner.current_token, cwd)
                .await;
            let kind = self.generators.kind_of(generator);
            candidates.extend(candidates_from(suggestions, kind, inner));
        }

        // Every generator here was a JavaScript callback. Some of them ask a
        // question Wisp can answer by reading a file.
        if javascript_only && let Some(native) = argument.native {
            candidates.extend(candidates_from(
                native.suggest(cwd, self.generators.history_limit()),
                SuggestionKind::Value,
                context,
            ));
        }
        candidates
    }
}

fn candidates_from(
    suggestions: Vec<Suggestion>,
    kind: SuggestionKind,
    context: &CompletionContext,
) -> Vec<Candidate> {
    let kind = match kind {
        SuggestionKind::Value => CandidateKind::Subcommand,
        SuggestionKind::Branch => CandidateKind::Branch,
        SuggestionKind::File => CandidateKind::File,
        SuggestionKind::Directory => CandidateKind::Directory,
    };
    suggestions
        .into_iter()
        .filter_map(|suggestion| {
            let insert = suggestion.insert.as_deref().unwrap_or(&suggestion.name);
            make_candidate(
                &suggestion.name,
                &shell_escape(insert),
                suggestion.description,
                kind,
                context,
            )
        })
        .collect()
}

fn suggestion_candidate(
    suggestion: &SuggestionSpec,
    fallback: CandidateKind,
    context: &CompletionContext,
) -> Option<Candidate> {
    if suggestion.presentation.hidden {
        return None;
    }
    let kind = match suggestion.kind {
        Some(SuggestionType::Folder) => CandidateKind::Directory,
        Some(SuggestionType::File) => CandidateKind::File,
        Some(SuggestionType::Subcommand) => CandidateKind::Subcommand,
        Some(SuggestionType::Option) => CandidateKind::Option,
        _ => fallback,
    };
    present(
        &suggestion.name,
        &suggestion.presentation,
        suggestion.description.clone(),
        kind,
        context,
    )
}

/// Fig lets a node ask for prefix matching where fuzzy matching would be
/// noise -- a list of file extensions, say.
fn apply_filter_strategy(candidates: &mut Vec<Candidate>, strategy: FilterStrategy, token: &str) {
    if strategy != FilterStrategy::Prefix || token.is_empty() {
        return;
    }
    let token = token.to_lowercase();
    candidates.retain(|candidate| candidate.label.to_lowercase().starts_with(&token));
}

/// Narrows the context to the part of the token a generator filters on.
fn query_context(
    generator: &GeneratorSpec,
    context: &CompletionContext,
) -> Option<CompletionContext> {
    let QueryTerm::AfterLast(separator) = generator.query_term.as_ref()?;
    let offset = context.current_token.rfind(separator.as_str())? + separator.len();
    Some(CompletionContext {
        command: context.command.clone(),
        args: context.args.clone(),
        current_token: context.current_token[offset..].to_owned(),
        replace_range: (context.replace_range.start + offset)..context.replace_range.end,
    })
}

/// Builds a candidate honouring Fig's display metadata: what the menu shows,
/// what gets inserted, and how far up the list it sits.
fn present(
    name: &str,
    presentation: &Presentation,
    description: Option<String>,
    kind: CandidateKind,
    context: &CompletionContext,
) -> Option<Candidate> {
    let label = presentation.display_name.as_deref().unwrap_or(name);
    let insert = presentation.insert_value.as_deref().unwrap_or(name);
    let mut candidate = make_candidate(label, insert, description, kind, context)?;
    if presentation.dangerous {
        // Fig flags what cannot be undone; say so where the description shows.
        candidate.description = Some(match candidate.description {
            Some(description) => format!("{DANGEROUS} {description}"),
            None => DANGEROUS.to_owned(),
        });
    }
    candidate.priority = presentation
        .priority
        .map_or(DEFAULT_PRIORITY, f64::from)
        .clamp(0.0, 100.0);
    Some(candidate)
}

/// Marks a suggestion Fig considers destructive.
const DANGEROUS: &str = "[dangerous]";

/// Fig's `{cursor}` marker has no place in the text handed to the shell.
fn strip_cursor_marker(value: &str) -> String {
    value.replace("{cursor}", "")
}

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

    fn additional_suggestions(self) -> &'a [SuggestionSpec] {
        match self {
            Self::Root(spec) => &spec.additional_suggestions,
            Self::Subcommand(spec) => &spec.additional_suggestions,
        }
    }

    fn requires_subcommand(self) -> bool {
        match self {
            Self::Root(spec) => spec.requires_subcommand,
            Self::Subcommand(spec) => spec.requires_subcommand,
        }
    }

    fn filter_strategy(self) -> FilterStrategy {
        match self {
            Self::Root(spec) => spec.filter_strategy,
            Self::Subcommand(spec) => spec.filter_strategy,
        }
    }

    fn parser_directives(self) -> Option<&'a ParserDirectives> {
        match self {
            Self::Root(spec) => spec.parser_directives.as_ref(),
            Self::Subcommand(spec) => spec.parser_directives.as_ref(),
        }
    }
}

struct ResolvedNode<'a> {
    node: NodeRef<'a>,
    /// The node one level up, which is where Fig's `help` template looks.
    parent: Option<NodeRef<'a>>,
    positional_index: usize,
    pending_argument: Option<&'a ArgumentSpec>,
    /// Options inherited from every ancestor that marked them persistent.
    persistent: Vec<&'a OptionSpec>,
    /// Option names already on the command line at this node.
    used: Vec<&'a str>,
    directives: Option<&'a ParserDirectives>,
    /// A consumed token whose argument requests alias expansion.
    alias_request: Option<AliasRequest<'a>>,
}

#[derive(Clone, Copy)]
struct AliasRequest<'a> {
    token_index: usize,
    resolver: &'a AliasResolverSpec,
}

impl<'a> ResolvedNode<'a> {
    /// The argument hiding behind an option's separator, as in
    /// `--message=<value>`, and where its value starts in the token.
    fn separated_argument(&self, token: &str) -> Option<(&'a ArgumentSpec, usize)> {
        if !is_option_token(token) {
            return None;
        }
        let (name, separator) = split_separator(token, self.directives)?;
        let option = find_option(self.node.options(), &self.persistent, name)?;
        let argument = option.arguments.first()?;
        Some((argument, name.len() + separator))
    }

    /// Some commands stop accepting options once a positional argument has
    /// been given, which Fig records as `optionsMustPrecedeArguments`.
    fn options_are_closed(&self) -> bool {
        self.positional_index > 0
            && self
                .directives
                .is_some_and(|directives| directives.options_must_precede_arguments)
    }

    /// Options still worth offering: not hidden, not already used unless they
    /// repeat, not excluded by something already typed, and not waiting on a
    /// dependency that is missing.
    fn option_candidates(&self, context: &CompletionContext) -> Vec<Candidate> {
        self.node
            .options()
            .iter()
            .chain(self.persistent.iter().copied())
            .filter(|option| self.is_available(option))
            .flat_map(|option| {
                let separator = option.requires_separator.as_deref().unwrap_or("");
                option.names.iter().filter_map(move |name| {
                    let insert = format!(
                        "{}{separator}",
                        option.presentation.insert_value.as_deref().unwrap_or(name)
                    );
                    let mut candidate = make_candidate(
                        option.presentation.display_name.as_deref().unwrap_or(name),
                        &strip_cursor_marker(&insert),
                        option.description.clone(),
                        CandidateKind::Option,
                        context,
                    )?;
                    candidate.priority = option
                        .presentation
                        .priority
                        .map_or(DEFAULT_PRIORITY, f64::from)
                        .clamp(0.0, 100.0);
                    // What the command cannot run without belongs at the top.
                    if option.required {
                        candidate.score += 0.05;
                    }
                    Some(candidate)
                })
            })
            .collect()
    }

    fn is_available(&self, option: &OptionSpec) -> bool {
        if option.presentation.hidden {
            return false;
        }
        let uses = self.used.iter().filter(|name| option.matches(name)).count();
        let allowed = match option.repeat {
            Repeat::Once => 1,
            Repeat::Many => usize::MAX,
            Repeat::Times(times) => times as usize,
        };
        if uses >= allowed {
            return false;
        }
        if option
            .exclusive_on
            .iter()
            .any(|name| self.used.contains(&name.as_str()))
        {
            return false;
        }
        option
            .depends_on
            .iter()
            .all(|name| self.used.contains(&name.as_str()))
    }
}

/// Once a variadic argument has started, Fig treats what follows as more of
/// its values rather than options, unless the argument opts out.
fn variadic_swallows_options(node: NodeRef<'_>, positional_index: usize) -> bool {
    argument_at(node.arguments(), positional_index)
        .is_some_and(|argument| argument.variadic && !argument.options_can_break_variadic)
        && positional_index > 0
}

/// A token is an option when it leads with a dash and is not a bare `-` or a
/// negative number a command might take as a value.
fn is_option_token(token: &str) -> bool {
    token.starts_with('-') && token != "-" && token != "--"
}

/// Splits `--name=value` into its name and the length of the separator.
fn split_separator<'t>(
    token: &'t str,
    directives: Option<&ParserDirectives>,
) -> Option<(&'t str, usize)> {
    let declared: Vec<&str> = directives
        .map(|directives| {
            directives
                .option_arg_separators
                .iter()
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();
    // `=` is what an option separator is, unless a spec says otherwise.
    let separators: &[&str] = if declared.is_empty() {
        &["="]
    } else {
        &declared
    };
    separators.iter().find_map(|separator| {
        token
            .find(separator)
            .filter(|index| *index > 0)
            .map(|index| (&token[..index], separator.len()))
    })
}

fn find_option<'a>(
    options: &'a [OptionSpec],
    persistent: &[&'a OptionSpec],
    name: &str,
) -> Option<&'a OptionSpec> {
    options
        .iter()
        .chain(persistent.iter().copied())
        .find(|option| option.matches(name))
}

fn argument_at(arguments: &[ArgumentSpec], index: usize) -> Option<&ArgumentSpec> {
    arguments
        .get(index)
        .or_else(|| arguments.last().filter(|argument| argument.variadic))
}

fn argument_alias(argument: &ArgumentSpec) -> Option<&AliasResolverSpec> {
    argument
        .parser_directives
        .as_ref()
        .and_then(|directives| directives.alias.as_ref())
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
    let mut commands = ZSH_BUILTINS
        .iter()
        .map(|command| (*command).to_owned())
        .collect::<Vec<_>>();
    seen.extend(commands.iter().cloned());
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

fn complete_commands(
    context: &CompletionContext,
    commands: &[String],
    cwd: &Path,
) -> Vec<Candidate> {
    commands
        .iter()
        .filter(|name| command_is_available(name, cwd))
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

const ZSH_BUILTINS: &[&str] = &[
    "alias", "bg", "builtin", "cd", "command", "dirs", "disown", "echo", "eval", "exec", "export",
    "false", "fc", "fg", "getopts", "hash", "history", "jobs", "popd", "print", "printf", "pushd",
    "pwd", "read", "set", "setopt", "source", "test", "true", "typeset", "ulimit", "umask",
    "unalias", "unset", "unsetopt", "wait", "whence", "which",
];

fn command_is_available(command: &str, cwd: &Path) -> bool {
    if ZSH_BUILTINS.contains(&command) {
        return true;
    }
    if command.contains('/') {
        let path = Path::new(command);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        return is_executable(&resolved);
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| is_executable(&directory.join(command)))
    })
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
    let home = std::env::var_os("HOME").map(PathBuf::from);
    complete_paths_with_home(context, cwd, directories_only, home.as_deref())
}

fn complete_paths_with_home(
    context: &CompletionContext,
    cwd: &Path,
    directories_only: bool,
    home: Option<&Path>,
) -> Vec<Candidate> {
    let token = &context.current_token;
    let token_path = Path::new(token);
    let resolved_token = if token == "~" {
        let Some(home) = home else {
            return Vec::new();
        };
        home.to_path_buf()
    } else if let Some(relative) = token.strip_prefix("~/") {
        let Some(home) = home else {
            return Vec::new();
        };
        home.join(relative)
    } else {
        cwd.join(token_path)
    };
    let (search_directory, display_parent, prefix) = if token.is_empty() {
        (cwd.to_path_buf(), PathBuf::new(), "")
    } else if token == "~" {
        (resolved_token, PathBuf::from("~"), "")
    } else if token.ends_with('/') {
        (resolved_token, token_path.to_path_buf(), "")
    } else {
        let search_directory = resolved_token
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.to_path_buf());
        let display_parent = token_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_default();
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
            make_candidate(&insert, &shell_escape_path(&insert), None, kind, context)
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
        priority: DEFAULT_PRIORITY,
        score,
        replace_start: context.replace_range.start,
        replace_end: context.replace_range.end,
    })
}

fn match_score(query: &str, candidate: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.5);
    }
    if candidate == query {
        return Some(1.0);
    }
    let query_lower = query.to_lowercase();
    let candidate_lower = candidate.to_lowercase();
    if candidate_lower == query_lower {
        return Some(0.99);
    }
    if candidate.starts_with(query) {
        return Some(0.9);
    }
    if candidate_lower.starts_with(&query_lower) {
        return Some(0.89);
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

fn shell_escape_path(value: &str) -> String {
    value.strip_prefix("~/").map_or_else(
        || shell_escape(value),
        |relative| format!("~/{}", shell_escape(relative)),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

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
    async fn cd_uses_the_imported_folder_template() {
        let specs = SpecStore::builtins();
        let spec = specs.get("cd").expect("cd spec");
        assert_eq!(spec.arguments[0].template, [Template::Folders]);

        let values = CompletionEngine::default()
            .complete(&snapshot("cd s"))
            .await;
        assert!(values.iter().any(|candidate| {
            candidate.label == "src/" && candidate.kind == CandidateKind::Directory
        }));
        assert!(
            values
                .iter()
                .all(|candidate| candidate.kind == CandidateKind::Directory)
        );

        let mut nested = snapshot("cd crates/");
        nested.cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf();
        let values = CompletionEngine::default().complete(&nested).await;
        assert!(values.iter().any(|candidate| {
            candidate.label == "crates/wisp-core/" && candidate.kind == CandidateKind::Directory
        }));

        let values = CompletionEngine::default().complete(&snapshot("cd ")).await;
        assert!(labels(&values).contains(&"src/"));
        assert!(!labels(&values).contains(&"wisp-core/"));
        assert!(!labels(&values).contains(&"-"));
        assert!(!labels(&values).contains(&"~"));
    }

    fn labels(candidates: &[Candidate]) -> Vec<&str> {
        candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect()
    }

    #[tokio::test]
    async fn a_separated_option_completes_its_value_after_the_separator() {
        let values = CompletionEngine::default()
            .complete(&snapshot("eza --color-scale=a"))
            .await;
        assert!(
            labels(&values).contains(&"all"),
            "got {:?}",
            labels(&values)
        );
        let all = values
            .iter()
            .find(|candidate| candidate.label == "all")
            .expect("the value is offered");
        // Only the text after `=` is replaced.
        assert_eq!(all.insert_text, "all");
        assert_eq!(all.replace_end - all.replace_start, 1);
    }

    #[tokio::test]
    async fn an_option_that_requires_a_separator_inserts_it() {
        let values = CompletionEngine::default()
            .complete(&snapshot("eza --color-sc"))
            .await;
        let option = values
            .iter()
            .find(|candidate| candidate.label == "--color-scale")
            .expect("the option is offered");
        assert_eq!(option.insert_text, "--color-scale=");
    }

    #[tokio::test]
    async fn an_excluded_option_disappears_once_its_counterpart_is_typed() {
        let engine = CompletionEngine::default();
        let offered = engine.complete(&snapshot("cal -")).await;
        assert!(labels(&offered).contains(&"-m"));

        let excluded = engine.complete(&snapshot("cal -y -")).await;
        assert!(
            !labels(&excluded).contains(&"-m"),
            "-m is exclusive on -y, got {:?}",
            labels(&excluded)
        );
    }

    #[tokio::test]
    async fn an_option_that_cannot_repeat_is_offered_only_once() {
        let engine = CompletionEngine::default();
        assert!(labels(&engine.complete(&snapshot("cal -")).await).contains(&"-y"));
        let repeated = engine.complete(&snapshot("cal -y -")).await;
        assert!(
            !labels(&repeated).contains(&"-y"),
            "got {:?}",
            labels(&repeated)
        );
    }

    #[tokio::test]
    async fn a_command_argument_restarts_completion_from_that_command() {
        let values = CompletionEngine::default()
            .complete(&snapshot("sudo git check"))
            .await;
        assert!(
            labels(&values).contains(&"checkout"),
            "expected git's subcommands after sudo, got {:?}",
            labels(&values)
        );
    }

    #[tokio::test]
    async fn hidden_suggestions_stay_out_of_the_menu() {
        let values = CompletionEngine::default()
            .complete(&snapshot("cargo build --"))
            .await;
        assert!(labels(&values).contains(&"--workspace"));
        // `--all` is a deprecated alias cargo's spec marks hidden.
        assert!(
            !labels(&values).contains(&"--all"),
            "got {:?}",
            labels(&values)
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("wisp-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the scratch directory");
        directory
    }

    #[tokio::test]
    async fn the_help_template_lists_the_sibling_subcommands() {
        let directory = scratch("help-template");
        // `ls` stands in for any installed command; the spec is what is tested.
        std::fs::write(
            directory.join("ls.ron"),
            r#"(
                version: 1,
                command: "ls",
                subcommands: [
                    (name: "build"),
                    (name: "deploy"),
                    (name: "help", arguments: [(name: "command", template: [Help])]),
                ],
            )"#,
        )
        .expect("write the spec");
        let mut specs = SpecStore::default();
        specs.load_dir(&directory).expect("load the spec");

        let values = CompletionEngine::new(specs)
            .complete(&snapshot("ls help "))
            .await;
        assert!(
            labels(&values).contains(&"build"),
            "got {:?}",
            labels(&values)
        );
        assert!(labels(&values).contains(&"deploy"));
    }

    #[tokio::test]
    async fn fig_priorities_order_equal_matches_and_are_clamped() {
        let directory = scratch("priority");
        std::fs::write(
            directory.join("ls.ron"),
            r#"(
                version: 1,
                command: "ls",
                subcommands: [
                    (name: "bottom", presentation: (priority: Some(1.0))),
                    (name: "default"),
                    (name: "top", presentation: (priority: Some(9000.0))),
                    (name: "apple", presentation: (priority: Some(1.0))),
                    (name: "astronomical", presentation: (priority: Some(100.0))),
                ],
            )"#,
        )
        .expect("write the spec");
        let mut specs = SpecStore::default();
        specs.load_dir(&directory).expect("load the spec");

        let values = CompletionEngine::new(specs)
            .complete(&snapshot("ls "))
            .await;
        assert_eq!(
            labels(&values),
            ["top", "astronomical", "default", "bottom", "apple"]
        );
        assert_eq!(values[0].priority, 100.0);
        assert_eq!(values[2].priority, 50.0);
        assert_eq!(values[4].priority, 1.0);

        let matching = CompletionEngine::new({
            let mut specs = SpecStore::default();
            specs.load_dir(&directory).expect("reload the spec");
            specs
        })
        .complete(&snapshot("ls a"))
        .await;
        assert_eq!(&labels(&matching)[..2], ["astronomical", "apple"]);
    }

    #[tokio::test]
    async fn equal_priority_candidates_keep_fig_source_order() {
        let directory = scratch("source-order");
        std::fs::write(
            directory.join("ls.ron"),
            r#"(
                version: 1,
                command: "ls",
                subcommands: [(name: "subcommand")],
                arguments: [(name: "value", suggestions: [(name: "argument")])],
                additional_suggestions: [(name: "additional")],
                options: [(names: ["--option"])],
            )"#,
        )
        .expect("write the spec");
        let mut specs = SpecStore::default();
        specs.load_dir(&directory).expect("load the spec");

        let values = CompletionEngine::new(specs)
            .complete(&snapshot("ls "))
            .await;
        assert_eq!(
            labels(&values),
            ["subcommand", "argument", "additional", "--option"]
        );
    }

    #[tokio::test]
    async fn accepted_suggestions_receive_persistent_recency_priority() {
        let directory = scratch("recency");
        std::fs::write(
            directory.join("ls.ron"),
            r#"(
                version: 1,
                command: "ls",
                subcommands: [(name: "alpha"), (name: "zulu")],
            )"#,
        )
        .expect("write the spec");
        let mut specs = SpecStore::default();
        specs.load_dir(&directory).expect("load the spec");
        let ranking = directory.join("ranking.json");
        let snapshot = snapshot("ls ");
        let engine =
            CompletionEngine::new(specs.clone()).with_recency_ranking(true, ranking.clone());
        let initial = engine.complete(&snapshot).await;
        assert_eq!(labels(&initial), ["alpha", "zulu"]);
        let selected = initial
            .iter()
            .find(|candidate| candidate.label == "zulu")
            .expect("zulu candidate");
        engine
            .record_selection(&snapshot, selected)
            .expect("persist recency");

        let reloaded = CompletionEngine::new(specs)
            .with_recency_ranking(true, ranking)
            .complete(&snapshot)
            .await;
        assert_eq!(reloaded[0].label, "zulu");
        assert!(reloaded[0].priority > 75.0);
    }

    #[tokio::test]
    async fn make_targets_stand_in_for_a_javascript_generator() {
        let directory = scratch("make-native");
        std::fs::write(
            directory.join("Makefile"),
            "build:\n\tcargo build\nlint:\n\tcargo clippy\n",
        )
        .expect("write the makefile");
        let mut snapshot = snapshot("make ");
        snapshot.cwd = directory;

        let values = CompletionEngine::default().complete(&snapshot).await;
        assert!(
            labels(&values).contains(&"build"),
            "got {:?}",
            labels(&values)
        );
        assert!(labels(&values).contains(&"lint"));
    }

    #[tokio::test]
    async fn an_imported_generator_completes_git_branches() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
        let inside_repository = std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(repository)
            .output()
            .is_ok_and(|output| output.status.success());
        if !inside_repository {
            return;
        }
        let mut snapshot = snapshot("git checkout ");
        snapshot.cwd = repository.to_path_buf();
        let values = CompletionEngine::default().complete(&snapshot).await;
        assert!(
            values
                .iter()
                .any(|candidate| candidate.kind == CandidateKind::Branch),
            "expected a branch from the imported generator, got {values:?}"
        );
    }

    #[tokio::test]
    async fn a_configured_git_alias_uses_the_canonical_subcommand_spec() {
        let directory = scratch("git-alias");
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&directory)
            .status()
            .is_ok_and(|status| status.success());
        if !initialized {
            return;
        }
        let configured = std::process::Command::new("git")
            .args(["config", "alias.wisp-checkout", "checkout"])
            .current_dir(&directory)
            .status()
            .expect("configure the repository alias");
        assert!(configured.success());

        let mut snapshot = snapshot("git wisp-checkout ");
        snapshot.cwd = directory;
        let values = CompletionEngine::default().complete(&snapshot).await;
        assert!(
            labels(&values).contains(&"-"),
            "expected checkout's last-branch suggestion, got {values:?}"
        );
        assert!(labels(&values).contains(&"--detach"));
        assert!(!labels(&values).contains(&"commit -m 'msg'"));
    }

    #[tokio::test]
    async fn an_alias_directive_expands_an_argument_at_a_nested_command_path() {
        let directory = scratch("nested-alias");
        std::fs::write(
            directory.join("package.json"),
            r#"{"scripts":{"switch":"git checkout"}}"#,
        )
        .expect("write package scripts");
        let engine = CompletionEngine::default();
        let spec = engine.specs.get("yarn").expect("built-in yarn spec");

        let expanded = engine
            .expand_runtime_aliases(spec, &["run".into(), "switch".into()], &directory)
            .await;
        assert_eq!(expanded, ["run", "git", "checkout"]);
    }

    #[tokio::test]
    async fn an_argument_without_an_alias_directive_is_never_rewritten() {
        let directory = scratch("plain-argument");
        std::fs::write(
            directory.join("package.json"),
            r#"{"scripts":{"switch":"git checkout"}}"#,
        )
        .expect("write package scripts");
        let spec = CommandSpec {
            command: "yarn".into(),
            arguments: vec![ArgumentSpec {
                name: "script".into(),
                ..ArgumentSpec::default()
            }],
            ..CommandSpec::default()
        };
        let engine = CompletionEngine::default();

        let expanded = engine
            .expand_runtime_aliases(&spec, &["switch".into()], &directory)
            .await;
        assert_eq!(expanded, ["switch"]);
    }

    #[test]
    fn unavailable_commands_are_filtered_from_command_candidates() {
        let unavailable = format!("wisp-command-that-does-not-exist-{}", std::process::id());
        let context = parse_completion_context(&unavailable, unavailable.chars().count());
        let values = complete_commands(
            &context,
            std::slice::from_ref(&unavailable),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        );
        assert!(values.is_empty());
    }

    #[test]
    fn zsh_builtins_are_available_without_path_executables() {
        assert!(command_is_available(
            "cd",
            Path::new(env!("CARGO_MANIFEST_DIR"))
        ));
    }

    #[test]
    fn tilde_paths_are_resolved_against_home_and_preserved_for_insertion() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wisp-tilde-test-{nonce}"));
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        std::fs::create_dir_all(home.join("My Folder")).unwrap();

        let documents = parse_completion_context("cd ~/Do", "cd ~/Do".chars().count());
        let values = complete_paths_with_home(&documents, &root, true, Some(&home));
        assert!(values.iter().any(|candidate| {
            candidate.label == "~/Documents/" && candidate.insert_text == "~/Documents/"
        }));

        let spaced = parse_completion_context("cd ~/My", "cd ~/My".chars().count());
        let values = complete_paths_with_home(&spaced, &root, true, Some(&home));
        assert!(values.iter().any(|candidate| {
            candidate.label == "~/My Folder/" && candidate.insert_text == "~/'My Folder/'"
        }));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bare_tilde_completes_home_children() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wisp-bare-tilde-test-{nonce}"));
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Projects")).unwrap();

        let context = parse_completion_context("cd ~", "cd ~".chars().count());
        let values = complete_paths_with_home(&context, &root, true, Some(&home));
        assert!(
            values
                .iter()
                .any(|candidate| candidate.label == "~/Projects/")
        );

        std::fs::remove_dir_all(root).unwrap();
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
