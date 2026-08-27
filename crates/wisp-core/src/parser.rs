use std::ops::Range;

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionContext {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub current_token: String,
    pub replace_range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    value: String,
    range: Range<usize>,
}

pub fn parse_completion_context(buffer: &str, cursor: usize) -> CompletionContext {
    let cursor = cursor.min(buffer.chars().count());
    let prefix: String = buffer.chars().take(cursor).collect();
    let mut tokens = tokenize(&prefix);

    let command_start = tokens
        .iter()
        .rposition(|token| matches!(token.value.as_str(), "|" | "||" | "&&" | ";"))
        .map_or(0, |index| index + 1);
    tokens.drain(..command_start);

    let ends_with_space = prefix.chars().last().is_some_and(char::is_whitespace);
    let (current_token, replace_range) = if ends_with_space || tokens.is_empty() {
        (String::new(), cursor..cursor)
    } else {
        let token = tokens.pop().expect("checked above");
        (token.value, token.range)
    };

    let command = tokens.first().map(|token| token.value.clone()).or_else(|| {
        if current_token.is_empty() {
            None
        } else {
            Some(current_token.clone())
        }
    });

    let args = if tokens.is_empty() {
        Vec::new()
    } else {
        tokens
            .into_iter()
            .skip(1)
            .map(|token| token.value)
            .collect()
    };

    CompletionContext {
        command,
        args,
        current_token,
        replace_range,
    }
}

fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }

        let start = index;
        if matches!(chars[index], '|' | '&' | ';') {
            let first = chars[index];
            index += 1;
            if index < chars.len() && chars[index] == first && first != ';' {
                index += 1;
            }
            tokens.push(Token {
                value: chars[start..index].iter().collect(),
                range: start..index,
            });
            continue;
        }

        let mut value = String::new();
        let mut quote = None;
        let mut escaped = false;
        while index < chars.len() {
            let ch = chars[index];
            if escaped {
                value.push(ch);
                escaped = false;
                index += 1;
                continue;
            }
            if ch == '\\' && quote != Some('\'') {
                escaped = true;
                index += 1;
                continue;
            }
            if matches!(ch, '\'' | '"') {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                } else {
                    value.push(ch);
                }
                index += 1;
                continue;
            }
            if quote.is_none() && (ch.is_whitespace() || matches!(ch, '|' | '&' | ';')) {
                break;
            }
            value.push(ch);
            index += 1;
        }
        if escaped {
            value.push('\\');
        }
        tokens.push(Token {
            value,
            range: start..index,
        });
    }

    tokens
}

/// Returns the cursor row/column relative to the start of the rendered prompt.
pub fn display_cursor(prompt: &str, buffer_before_cursor: &str, columns: u16) -> (u16, u16) {
    let columns = usize::from(columns.max(1));
    let prompt = strip_ansi_escapes::strip_str(prompt);
    let mut row = 0usize;
    let mut column = 0usize;

    for ch in prompt.chars().chain(buffer_before_cursor.chars()) {
        match ch {
            '\n' => {
                row += 1;
                column = 0;
            }
            '\r' => column = 0,
            _ => {
                let width = ch.width().unwrap_or(0);
                if width > 0 && column + width > columns {
                    row += 1;
                    column = 0;
                }
                column += width;
                if column == columns {
                    row += 1;
                    column = 0;
                }
            }
        }
    }

    (
        row.min(u16::MAX as usize) as u16,
        column.min(u16::MAX as usize) as u16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_incomplete_quotes_and_current_token() {
        let context = parse_completion_context("git checkout 'fea", 17);
        assert_eq!(context.command.as_deref(), Some("git"));
        assert_eq!(context.args, ["checkout"]);
        assert_eq!(context.current_token, "fea");
        assert_eq!(context.replace_range, 13..17);
    }

    #[test]
    fn resets_command_after_pipeline() {
        let context = parse_completion_context("printf foo | rg ma", 18);
        assert_eq!(context.command.as_deref(), Some("rg"));
        assert_eq!(context.current_token, "ma");
    }

    #[test]
    fn cursor_layout_handles_ansi_unicode_and_wrapping() {
        assert_eq!(display_cursor("\u{1b}[32mλ\u{1b}[0m ", "你好ab", 8), (1, 0));
    }
}
