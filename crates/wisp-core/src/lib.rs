mod engine;
mod generator;
mod native;
mod parser;
mod ranking;
mod spec;

pub use engine::CompletionEngine;
pub use generator::{GeneratorRuntime, Suggestion, SuggestionKind};
pub use native::Native;
pub use parser::{CompletionContext, display_cursor, parse_completion_context};
pub use spec::{
    ArgumentSpec, CacheSpec, CommandSpec, FilterStrategy, GeneratorSpec, OptionSpec,
    ParserDirectives, Presentation, QueryTerm, Repeat, SpecStore, SubcommandSpec, SuggestionSpec,
    SuggestionType, Template, Trigger,
};
