//! Text plan parser implementation
//! 
//! Handles parsing of PostgreSQL text format execution plans

use crate::{PlanLine};
use crate::plan_parser::PlanParser as LegacyPlanParser;
use crate::parsing::parser_trait::{PlanParser, PlanParserCore, ParseMetadata, ParsedPlanResult, PlanSourceFormat};
use crate::parsing::errors::{ParseError, ParseResult};
use regex::Regex;

/// Parser for text format PostgreSQL execution plans
pub struct TextPlanParser {
    /// Regex to detect plan nodes with cost information
    plan_node_regex: Regex,
}

impl TextPlanParser {
    /// Create a new text plan parser
    pub fn new() -> ParseResult<Self> {
        let plan_node_regex = Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)")
            .map_err(|e| ParseError::RegexError {
                message: "Failed to compile plan node regex".to_string(),
                pattern: r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)".to_string(),
            })?;

        Ok(Self { plan_node_regex })
    }

    /// Check if content contains PostgreSQL plan node patterns
    fn has_plan_pattern(&self, input: &str) -> bool {
        self.plan_node_regex.is_match(input)
    }

    /// Convert text input to structured plan lines
    fn parse_text_lines(input: &str) -> Vec<PlanLine> {
        input
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PlanLine::new(line))
            .collect()
    }

    /// Check if input is likely JSON format (to avoid false positives)
    fn looks_like_json(input: &str) -> bool {
        let trimmed = input.trim_start();
        trimmed.starts_with('[') || trimmed.starts_with('{')
    }
}

impl Default for TextPlanParser {
    fn default() -> Self {
        Self::new().expect("Failed to create default TextPlanParser")
    }
}

impl PlanParserCore for TextPlanParser {
    fn can_parse(&self, input: &str) -> bool {
        // Reject if it looks like JSON
        if Self::looks_like_json(input) {
            return false;
        }

        // Check for plan patterns or accept any non-JSON text as potentially parseable
        // This is more permissive since text plans can have various formats
        self.has_plan_pattern(input) || !input.trim().is_empty()
    }

    fn parse(&self, input: &str, _metadata: ParseMetadata) -> ParseResult<ParsedPlanResult> {
        if Self::looks_like_json(input) {
            return Err(ParseError::FormatDetectionError {
                message: "Input appears to be JSON format, not text".to_string(),
                content_preview: input.chars().take(100).collect(),
            });
        }

        if input.trim().is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "text plan content".to_string(),
            });
        }

        // Convert to plan lines
        let plan_lines = Self::parse_text_lines(input);
        
        if plan_lines.is_empty() {
            return Err(ParseError::EmptyInput {
                expected: "non-empty plan lines".to_string(),
            });
        }

        // Use existing plan parser to create ParsedPlan
        let legacy_parser = LegacyPlanParser::new()
            .map_err(|e| ParseError::InvalidNodeStructure {
                message: "Failed to create legacy plan parser".to_string(),
                context: format!("{:?}", e),
            })?;

        let parsed_plan = legacy_parser.parse_plan_from_lines(&plan_lines)
            .map_err(|e| ParseError::InvalidNodeStructure {
                message: "Failed to parse text plan".to_string(),
                context: format!("{:?}", e),
            })?;

        let mut warnings = Vec::new();
        
        // Add warning if no plan patterns detected
        if !self.has_plan_pattern(input) {
            warnings.push("No cost information patterns detected in text plan".to_string());
        }

        Ok(ParsedPlanResult {
            parsed_plan,
            source_format: PlanSourceFormat::Text,
            warnings,
        })
    }

    fn format_name(&self) -> &'static str {
        "text"
    }

    fn priority(&self) -> u8 {
        100 // Standard priority, lower than JSON parser
    }

    fn description(&self) -> &'static str {
        "PostgreSQL text format execution plan parser"
    }
}

impl PlanParser for TextPlanParser {
    type Builder = crate::parsing::TextPlanBuilder;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_text_format_detection() {
        let parser = TextPlanParser::new().unwrap();
        
        // Should accept text plans
        assert!(parser.can_parse("Seq Scan on users  (cost=0.00..10.00 rows=100 width=8)"));
        assert!(parser.can_parse("Some text without cost info"));
        assert!(parser.can_parse("  Index Scan using pk_users  "));
        
        // Should reject JSON
        assert!(!parser.can_parse(r#"[{"Plan": {}}]"#));
        assert!(!parser.can_parse(r#"{"Plan": {}}"#));
        
        // Should reject empty
        assert!(!parser.can_parse(""));
        assert!(!parser.can_parse("   "));
    }

    #[test]
    fn test_parser_properties() {
        let parser = TextPlanParser::new().unwrap();
        
        assert_eq!(parser.format_name(), "text");
        assert_eq!(parser.priority(), 100);
        assert!(parser.description().contains("text"));
    }

    #[test]
    fn test_plan_pattern_detection() {
        let parser = TextPlanParser::new().unwrap();
        
        assert!(parser.has_plan_pattern("Seq Scan on users  (cost=0.00..10.00 rows=100 width=8)"));
        assert!(parser.has_plan_pattern("Some text\n  ->  Index Scan  (cost=0.42..8.44 rows=1 width=16)"));
        assert!(!parser.has_plan_pattern("Just some text without cost info"));
    }

    #[test]
    fn test_parse_simple_text_plan() {
        let parser = TextPlanParser::new().unwrap();
        
        let text_content = r#"Seq Scan on users  (cost=0.00..10.00 rows=100 width=8)
  Output: id, name
  Filter: (active = true)"#;

        let metadata = ParseMetadata::new(
            Utc::now(),
            100.5,
            "SELECT * FROM users WHERE active = true".to_string(),
        );

        let result = parser.parse(text_content, metadata);
        assert!(result.is_ok());
        
        let parsed_result = result.unwrap();
        assert_eq!(parsed_result.source_format, PlanSourceFormat::Text);
        // Should have no warnings since cost patterns are present
        assert!(parsed_result.warnings.is_empty());
    }

    #[test]
    fn test_parse_text_without_cost_patterns() {
        let parser = TextPlanParser::new().unwrap();
        
        let text_content = "Some execution plan text without cost information";
        let metadata = ParseMetadata::new(
            Utc::now(),
            50.0,
            "SELECT 1".to_string(),
        );

        let result = parser.parse(text_content, metadata);
        assert!(result.is_ok());
        
        let parsed_result = result.unwrap();
        assert_eq!(parsed_result.source_format, PlanSourceFormat::Text);
        // Should have warning about missing cost patterns
        assert!(!parsed_result.warnings.is_empty());
        assert!(parsed_result.warnings[0].contains("cost information"));
    }

    #[test]
    fn test_parse_json_input_rejection() {
        let parser = TextPlanParser::new().unwrap();
        
        let json_input = r#"[{"Plan": {"Node Type": "Seq Scan"}}]"#;
        let metadata = ParseMetadata::new(
            Utc::now(),
            100.0,
            "SELECT * FROM users".to_string(),
        );

        let result = parser.parse(json_input, metadata);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::FormatDetectionError { .. }));
    }

    #[test]
    fn test_parse_empty_input() {
        let parser = TextPlanParser::new().unwrap();
        
        let empty_input = "";
        let metadata = ParseMetadata::new(
            Utc::now(),
            100.0,
            "SELECT 1".to_string(),
        );

        let result = parser.parse(empty_input, metadata);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::EmptyInput { .. }));
    }

    #[test]
    fn test_complex_nested_plan() {
        let parser = TextPlanParser::new().unwrap();
        
        let complex_plan = r#"Nested Loop  (cost=1.15..279.82 rows=7 width=110)
  Output: m."Id", m."Name"
  ->  Index Scan using "IX_Test1" on "Shared"."Test1" m  (cost=0.57..2.79 rows=1 width=54)
        Output: m."Id", m."Name"
        Index Cond: (m."Id" = 1)
  ->  Index Scan using "IX_Test2" on "Shared"."Test2" t  (cost=0.57..274.10 rows=292 width=56)
        Output: t."Id", t."Value"
        Index Cond: (t."TestId" = m."Id")"#;

        let metadata = ParseMetadata::new(
            Utc::now(),
            279.82,
            "SELECT m.Id, m.Name FROM Test1 m JOIN Test2 t ON t.TestId = m.Id WHERE m.Id = 1".to_string(),
        );

        let result = parser.parse(complex_plan, metadata);
        assert!(result.is_ok());
        
        let parsed_result = result.unwrap();
        assert_eq!(parsed_result.source_format, PlanSourceFormat::Text);
        assert!(parsed_result.warnings.is_empty());
    }
}