//! Plan format detection logic
//!
//! Centralizes the logic for detecting whether plan content is JSON or text format

use crate::parsing::errors::{ParseError, ParseResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanFormat {
    Text,
    Json,
}

/// Streaming heuristic: does this line look like the start of a JSON plan?
///
/// Used while consuming a log line-by-line, where the full document is not
/// yet available so real JSON validation is impossible. A `true` here is a
/// hypothesis, not a verdict — the JSON builder demotes back to query text
/// if the accumulated content turns out not to be a plan document.
pub fn looks_like_json_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('[') || trimmed.starts_with('{')
}

/// Detects the format of plan content
pub fn detect_plan_format(content: &str) -> ParseResult<PlanFormat> {
    let trimmed = content.trim_start();

    if trimmed.is_empty() {
        return Err(ParseError::EmptyInput {
            expected: "plan content".to_string(),
        });
    }

    // JSON format detection
    if looks_like_json_start(trimmed) {
        // Validate that it's actually parseable JSON
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(_) => Ok(PlanFormat::Json),
            Err(_) => Err(ParseError::FormatDetectionError {
                message: "Content starts with JSON markers but is not valid JSON".to_string(),
                content_preview: get_content_preview(content),
            }),
        }
    } else {
        // Assume text format for anything else
        // Could add more validation here (e.g., looking for cost patterns)
        Ok(PlanFormat::Text)
    }
}

/// Get a preview of content for error messages (first 100 characters).
///
/// Takes 100 *characters*, not bytes: `&content[..100]` panics when byte 100
/// lands inside a multi-byte character, which any non-ASCII text in a quoted
/// identifier, comment, or literal will produce. The text and JSON parsers
/// already use `chars().take(100)` for their equivalent previews.
fn get_content_preview(content: &str) -> String {
    const PREVIEW_LENGTH: usize = 100;
    let preview: String = content.chars().take(PREVIEW_LENGTH).collect();
    if preview.len() == content.len() {
        preview
    } else {
        format!("{preview}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_format_detection() {
        let json_content = r#"[{"Plan": {"Node Type": "Seq Scan"}}]"#;
        assert_eq!(detect_plan_format(json_content).unwrap(), PlanFormat::Json);

        let json_object = r#"{"Plan": {"Node Type": "Seq Scan"}}"#;
        assert_eq!(detect_plan_format(json_object).unwrap(), PlanFormat::Json);
    }

    #[test]
    fn test_text_format_detection() {
        let text_content = r#"Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)"#;
        assert_eq!(detect_plan_format(text_content).unwrap(), PlanFormat::Text);
    }

    #[test]
    fn test_empty_input() {
        let result = detect_plan_format("");
        assert!(matches!(result, Err(ParseError::EmptyInput { .. })));

        let result = detect_plan_format("   ");
        assert!(matches!(result, Err(ParseError::EmptyInput { .. })));
    }

    #[test]
    fn test_invalid_json() {
        let invalid_json = "[{invalid json";
        let result = detect_plan_format(invalid_json);
        assert!(matches!(
            result,
            Err(ParseError::FormatDetectionError { .. })
        ));
    }
}
