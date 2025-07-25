//! Unified error handling for parsing operations

use std::fmt;

pub type ParseResult<T> = Result<T, ParseError>;

#[derive(Debug, Clone)]
pub enum ParseError {
    /// Invalid cost format in plan text
    InvalidCostFormat {
        message: String,
        line: String,
    },
    /// Invalid node structure  
    InvalidNodeStructure {
        message: String,
        context: String,
    },
    /// Regex compilation or matching error
    RegexError {
        message: String,
        pattern: String,
    },
    /// Invalid indentation in plan text
    InvalidIndentation {
        message: String,
        line: String,
        expected_level: usize,
        actual_level: usize,
    },
    /// Empty or missing input
    EmptyInput {
        expected: String,
    },
    /// Invalid JSON format
    InvalidJsonFormat {
        message: String,
        json_error: String,
    },
    /// Missing required JSON plan data
    MissingJsonPlanData {
        message: String,
        field: String,
    },
    /// Log parsing error
    LogParsingError {
        message: String,
        line_number: Option<usize>,
        line_content: String,
    },
    /// Format detection error
    FormatDetectionError {
        message: String,
        content_preview: String,
    },
    /// Builder state error
    BuilderStateError {
        message: String,
        current_state: String,
        expected_state: String,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::InvalidCostFormat { message, line } => {
                write!(f, "Invalid cost format: {} (line: '{}')", message, line)
            }
            ParseError::InvalidNodeStructure { message, context } => {
                write!(f, "Invalid node structure: {} (context: '{}')", message, context)
            }
            ParseError::RegexError { message, pattern } => {
                write!(f, "Regex error: {} (pattern: '{}')", message, pattern)
            }
            ParseError::InvalidIndentation { message, line, expected_level, actual_level } => {
                write!(f, "Invalid indentation: {} (line: '{}', expected: {}, actual: {})", 
                       message, line, expected_level, actual_level)
            }
            ParseError::EmptyInput { expected } => {
                write!(f, "Empty input provided, expected: {}", expected)
            }
            ParseError::InvalidJsonFormat { message, json_error } => {
                write!(f, "Invalid JSON format: {} (JSON error: {})", message, json_error)
            }
            ParseError::MissingJsonPlanData { message, field } => {
                write!(f, "Missing JSON plan data: {} (field: '{}')", message, field)
            }
            ParseError::LogParsingError { message, line_number, line_content } => {
                if let Some(line_num) = line_number {
                    write!(f, "Log parsing error at line {}: {} (content: '{}')", 
                           line_num, message, line_content)
                } else {
                    write!(f, "Log parsing error: {} (content: '{}')", message, line_content)
                }
            }
            ParseError::FormatDetectionError { message, content_preview } => {
                write!(f, "Format detection error: {} (content: '{}')", message, content_preview)
            }
            ParseError::BuilderStateError { message, current_state, expected_state } => {
                write!(f, "Builder state error: {} (current: '{}', expected: '{}')", 
                       message, current_state, expected_state)
            }
        }
    }
}

impl std::error::Error for ParseError {}

impl From<regex::Error> for ParseError {
    fn from(err: regex::Error) -> Self {
        ParseError::RegexError {
            message: err.to_string(),
            pattern: "unknown".to_string(),
        }
    }
}

impl From<serde_json::Error> for ParseError {
    fn from(err: serde_json::Error) -> Self {
        ParseError::InvalidJsonFormat {
            message: "JSON deserialization failed".to_string(),
            json_error: err.to_string(),
        }
    }
}