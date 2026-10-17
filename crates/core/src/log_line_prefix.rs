//! Extraction of the database name from a PostgreSQL `log_line_prefix`.
//!
//! The log splitter already consumes the leading timestamp (`%m` or `%t`), so the
//! prefix is compiled into a pattern for what follows it. Only auto_explain's
//! `duration:` lines are matched against it, which keeps it off the hot path.

use anyhow::{Result, bail};
use regex::Regex;

/// PostgreSQL's own default, used when none is configured.
pub const DEFAULT_LOG_LINE_PREFIX: &str = "%m [%p] ";

/// Label for queries whose database could not be determined.
pub const UNKNOWN_DATABASE: &str = "unknown";

#[derive(Debug, Clone)]
pub struct LogLinePrefix {
    format: String,
    tail: Option<Regex>,
}

impl LogLinePrefix {
    /// Compile `format`. It must start with `%m` or `%t`, the timestamp the log
    /// splitter keys on, and may only use escapes PostgreSQL defines.
    pub fn parse(format: &str) -> Result<Self> {
        let rest = format
            .strip_prefix("%m")
            .or_else(|| format.strip_prefix("%t"))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "log_line_prefix {format:?} must start with %m or %t; lines are located by their timestamp"
                )
            })?;
        let tokens = tokenize(rest)?;
        let has_database = tokens
            .iter()
            .any(|token| matches!(token, Token::Escape { code: 'd', .. }));
        let tail = if has_database {
            Some(Regex::new(&tail_pattern(&tokens))?)
        } else {
            None
        };
        Ok(Self {
            format: format.to_string(),
            tail,
        })
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub fn has_database(&self) -> bool {
        self.tail.is_some()
    }

    /// The database name in `message`, the text after the timestamp, or `None`
    /// when the prefix has no `%d`, does not match, or names no database.
    pub fn database<'a>(&self, message: &'a str) -> Option<&'a str> {
        let name = self
            .tail
            .as_ref()?
            .captures(message)?
            .name("db")?
            .as_str()
            .trim();
        (!name.is_empty() && name != "[unknown]").then_some(name)
    }
}

impl Default for LogLinePrefix {
    fn default() -> Self {
        Self::parse(DEFAULT_LOG_LINE_PREFIX).expect("the default prefix is valid")
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Literal(char),
    Escape { code: char, padded: bool },
}

fn tokenize(format: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            tokens.push(Token::Literal(ch));
            continue;
        }
        let mut padded = false;
        while chars
            .peek()
            .is_some_and(|c| *c == '-' || c.is_ascii_digit())
        {
            padded = true;
            chars.next();
        }
        match chars.next() {
            Some('%') => tokens.push(Token::Literal('%')),
            Some(
                code @ ('a' | 'u' | 'd' | 'r' | 'h' | 'b' | 'p' | 'P' | 't' | 'm' | 'n' | 'i' | 'e'
                | 'c' | 'l' | 's' | 'v' | 'x' | 'Q'),
            ) => tokens.push(Token::Escape { code, padded }),
            // Session-only marker: the duration lines this is matched against are
            // always session lines, so everything after %q applies.
            Some('q') => {}
            Some(other) => bail!("unsupported log_line_prefix escape %{other}"),
            None => bail!("log_line_prefix ends with a lone %"),
        }
    }
    Ok(tokens)
}

fn tail_pattern(tokens: &[Token]) -> String {
    let mut pattern = String::from("^");
    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Literal(ch) => pattern.push_str(&regex::escape(&ch.to_string())),
            Token::Escape { code, padded } => {
                // A lazy match needs a literal after it to stop at.
                let followed_by_literal = matches!(tokens.get(index + 1), Some(Token::Literal(_)));
                let free_text = if followed_by_literal { ".*?" } else { r"\S*" };
                let group = match code {
                    'd' => format!("(?P<db>{free_text})"),
                    'a' | 'u' | 'r' | 'h' | 'b' | 'i' => format!("(?:{free_text})"),
                    'p' | 'P' | 'l' | 'x' => r"\d+".to_string(),
                    'Q' => r"-?\d+".to_string(),
                    'e' => r"\w{5}".to_string(),
                    'c' => r"[0-9a-f]+\.[0-9a-f]+".to_string(),
                    'v' => r"\S*".to_string(),
                    's' | 't' | 'm' => {
                        r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:\.\d+)?(?: [A-Z+\-][\w:]*)?"
                            .to_string()
                    }
                    'n' => r"\d+\.\d+".to_string(),
                    _ => unreachable!("tokenize only emits known escapes"),
                };
                if *padded {
                    pattern.push_str(&format!(r"\s*{group}\s*"));
                } else {
                    pattern.push_str(&group);
                }
            }
        }
    }
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(format: &str, message: &str) -> Option<String> {
        LogLinePrefix::parse(format)
            .unwrap()
            .database(message)
            .map(str::to_string)
    }

    #[test]
    fn default_prefix_has_no_database() {
        let prefix = LogLinePrefix::default();
        assert!(!prefix.has_database());
        assert_eq!(prefix.database(" [123] LOG:  duration: 1 ms"), None);
    }

    #[test]
    fn extracts_user_at_database() {
        assert_eq!(
            db(
                "%m [%p] %q%u@%d ",
                " [123] alice@shop LOG:  duration: 1 ms  plan:"
            ),
            Some("shop".to_string())
        );
    }

    #[test]
    fn extracts_a_database_followed_by_other_escapes() {
        assert_eq!(
            db(
                "%m [%p] db=%d,user=%u,app=%a ",
                " [9] db=orders,user=bob,app=psql shell LOG:  x"
            ),
            Some("orders".to_string())
        );
    }

    #[test]
    fn timestamp_escapes_mid_format_are_matched() {
        assert_eq!(
            db("%t [%p]: %d ", " [5]: crm LOG:  duration: 2 ms"),
            Some("crm".to_string())
        );
    }

    #[test]
    fn padded_escapes_are_accepted() {
        assert_eq!(
            db("%m %-6p %d ", " 42     main LOG:  duration: 1 ms"),
            Some("main".to_string())
        );
    }

    #[test]
    fn unknown_or_empty_database_is_none() {
        assert_eq!(db("%m [%p] %u@%d ", " [1] alice@[unknown] LOG:  x"), None);
        assert_eq!(db("%m [%p] %u@%d ", " [1] alice@ LOG:  x"), None);
    }

    #[test]
    fn a_non_matching_message_is_none() {
        assert_eq!(db("%m [%p] %u@%d ", "garbage"), None);
    }

    #[test]
    fn format_must_start_with_a_timestamp() {
        assert!(LogLinePrefix::parse("%p %m %d ").is_err());
        assert!(LogLinePrefix::parse("%m %z ").is_err());
        assert!(LogLinePrefix::parse("%m %").is_err());
        assert!(LogLinePrefix::parse("%t %d").is_ok());
    }
}
