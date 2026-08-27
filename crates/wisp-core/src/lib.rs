mod engine;
mod parser;
mod spec;

pub use engine::CompletionEngine;
pub use parser::{CompletionContext, display_cursor, parse_completion_context};
pub use spec::{
    ArgumentSpec, CommandSpec, ImportedGenerator, OptionSpec, SpecStore, SubcommandSpec,
    SuggestionSource,
};
