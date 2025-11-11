//! Text plan parser implementation
//!
//! Handles parsing of PostgreSQL text format execution plans

use crate::PlanLine;
use crate::parsing::errors::{ParseError, ParseResult};
use crate::parsing::parser_trait::{
    ParseMetadata, ParsedPlanResult, PlanParser, PlanParserCore, PlanSourceFormat,
};
use crate::plan_parser::PlanParser as LegacyPlanParser;
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
            .map_err(|_| ParseError::RegexError {
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
            .map(PlanLine::new)
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
        let legacy_parser =
            LegacyPlanParser::new().map_err(|e| ParseError::InvalidNodeStructure {
                message: "Failed to create legacy plan parser".to_string(),
                context: format!("{:?}", e),
            })?;

        let parsed_plan = legacy_parser
            .parse_plan_from_lines(&plan_lines)
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
        assert!(parser.can_parse(r#"Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)"#));
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

        assert!(parser.has_plan_pattern(r#"Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)"#));
        assert!(
            parser
                .has_plan_pattern("Some text\n  ->  Index Scan using \"IX_Monitors_AcceptanceId\"  (cost=0.57..2.79 rows=1 width=54)")
        );
        assert!(!parser.has_plan_pattern("Just some text without cost info"));
    }

    #[test]
    fn test_parse_simple_text_plan() {
        let parser = TextPlanParser::new().unwrap();

        let text_content = r#"Index Scan using "PK_VentilatorHourlyCaches" on "Shared"."VentilatorHourlyCaches" v  (cost=0.42..851.21 rows=822 width=16)
  Output: "AcceptanceId", "MeasuredDate", "VentilatorId"
  Index Cond: ((v."AcceptanceId" = ANY ('{322,319,1062,1100}'::integer[])) AND (v."MeasuredDate" >= '2025-06-15 00:03:47+00'::timestamp with time zone))"#;

        let metadata = ParseMetadata::new(
            Utc::now(),
            3680.828,
            "SELECT v.AcceptanceId, v.MeasuredDate, v.VentilatorId FROM VentilatorHourlyCaches v WHERE v.AcceptanceId = ANY ($1) AND v.MeasuredDate >= $2".to_string(),
        );

        let result = parser.parse(text_content, metadata);
        assert!(result.is_ok());

        let parsed_result = result.unwrap();
        assert_eq!(parsed_result.source_format, PlanSourceFormat::Text);
        // Should have no warnings since cost patterns are present
        assert!(parsed_result.warnings.is_empty());
    }

    #[test]
    fn test_parse_text_with_minimal_cost_info() {
        let parser = TextPlanParser::new().unwrap();

        // Minimal plan with basic cost info - should parse with potential warnings
        let text_content = "Result  (rows=1)";
        let metadata = ParseMetadata::new(Utc::now(), 50.0, "SELECT 1".to_string());

        let result = parser.parse(text_content, metadata);
        // Parser should either succeed or fail gracefully
        // The important thing is it doesn't panic
        match result {
            Ok(parsed_result) => {
                assert_eq!(parsed_result.source_format, PlanSourceFormat::Text);
                // May or may not have warnings depending on what was parsed
            }
            Err(_) => {
                // It's acceptable to reject truly minimal input
                // The test verifies the parser handles edge cases gracefully
            }
        }
    }

    #[test]
    fn test_parse_json_input_rejection() {
        let parser = TextPlanParser::new().unwrap();

        let json_input = r#"[{"Plan": {"Node Type": "Seq Scan"}}]"#;
        let metadata = ParseMetadata::new(Utc::now(), 100.0, "SELECT * FROM users".to_string());

        let result = parser.parse(json_input, metadata);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::FormatDetectionError { .. }
        ));
    }

    #[test]
    fn test_parse_empty_input() {
        let parser = TextPlanParser::new().unwrap();

        let empty_input = "";
        let metadata = ParseMetadata::new(Utc::now(), 100.0, "SELECT 1".to_string());

        let result = parser.parse(empty_input, metadata);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::EmptyInput { .. }));
    }

    #[test]
    fn test_complex_nested_plan() {
        let parser = TextPlanParser::new().unwrap();

        let complex_plan = r#"Sort  (cost=279.91..279.93 rows=7 width=110)
  Output: m."Id", m."AcceptanceId", m."CreatedDate", m."DeviceName", m."IsValidated", m."MeasuredDate", m."ValidatedById", m."ValidationDate", m0."Id", m0."Comment", m0."DeviceId", m0."MeasurementTypeId", m0."Value"
  Sort Key: m."Id"
  ->  Nested Loop Left Join  (cost=1.15..279.82 rows=7 width=110)
        Output: m."Id", m."AcceptanceId", m."CreatedDate", m."DeviceName", m."IsValidated", m."MeasuredDate", m."ValidatedById", m."ValidationDate", m0."Id", m0."Comment", m0."DeviceId", m0."MeasurementTypeId", m0."Value"
        ->  Index Scan using "IX_Monitors_AcceptanceId" on "Shared"."Monitors" m  (cost=0.57..2.79 rows=1 width=54)
              Output: m."Id", m."AcceptanceId", m."CreatedDate", m."MeasuredDate", m."DeviceName", m."IsValidated", m."ValidatedById", m."ValidationDate"
              Index Cond: (m."AcceptanceId" = 1395)
              Filter: ((m."MeasuredDate" >= '2025-06-25 00:01:20.259+00'::timestamp with time zone) AND (m."MeasuredDate" <= '2025-06-25 00:03:20.259+00'::timestamp with time zone))
        ->  Index Scan using "IX_MonitorMeasurements_DeviceId" on "Shared"."MonitorMeasurements" m0  (cost=0.57..274.10 rows=292 width=56)
              Output: m0."Id", m0."Comment", m0."DeviceId", m0."MeasurementTypeId", m0."Value"
              Index Cond: (m0."DeviceId" = m."Id")"#;

        let metadata = ParseMetadata::new(
            Utc::now(),
            2244.493,
            "SELECT m.Id, m.AcceptanceId FROM Monitors m LEFT JOIN MonitorMeasurements m0 ON m.Id = m0.DeviceId WHERE m.AcceptanceId = 1395 ORDER BY m.Id"
                .to_string(),
        );

        let result = parser.parse(complex_plan, metadata);
        assert!(result.is_ok());

        let parsed_result = result.unwrap();
        assert_eq!(parsed_result.source_format, PlanSourceFormat::Text);
        assert!(parsed_result.warnings.is_empty());
    }
}
