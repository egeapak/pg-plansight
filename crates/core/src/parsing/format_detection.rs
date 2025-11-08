//! Plan format detection logic
//!
//! Centralizes the logic for detecting whether plan content is JSON or text format

use crate::parsing::errors::{ParseError, ParseResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanFormat {
    Text,
    Json,
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
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
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

/// Get a preview of content for error messages (first 100 characters)
fn get_content_preview(content: &str) -> String {
    const PREVIEW_LENGTH: usize = 100;
    if content.len() <= PREVIEW_LENGTH {
        content.to_string()
    } else {
        format!("{}...", &content[..PREVIEW_LENGTH])
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
        let text_content = "Seq Scan on users  (cost=0.00..10.00 rows=100 width=8)";
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
