use crate::ParsedPlan;
use chrono::Utc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_child_node_parsing() {
        // Test plan that should have child nodes
        let plan_text = r#"Nested Loop  (cost=0.43..11444.89 rows=1 width=72)
  Output: b."Id", b."AcceptanceId", b."CreatedDate", b."DeviceName", b."IsActive", b."MeasuredDate", b."SampleType", b."TestId"
  Inner Unique: true
  Join Filter: (b."AcceptanceId" = a."Id")
  ->  Index Scan using "IX_BloodGasDevices_AcceptanceId" on "Shared"."BloodGasDevices" b  (cost=0.43..11402.07 rows=1 width=72)
        Output: b."Id", b."AcceptanceId", b."CreatedDate", b."DeviceName", b."IsActive", b."MeasuredDate", b."SampleType", b."TestId"
        Index Cond: (b."AcceptanceId" = ANY ('{840,1008,1348,1387}'::integer[]))
        Filter: (b."IsActive" AND (b."MeasuredDate" >= '2025-06-20 00:00:48.53891+00'::timestamp with time zone))
  ->  Seq Scan on "Shared"."Acceptances" a  (cost=0.00..29.03 rows=1103 width=4)
        Output: a."Id""#;

        println!("Testing ParsedPlan::from_text_plan directly...");

        // Test direct parsing
        let parsed_plan = ParsedPlan::from_text_plan(plan_text).unwrap();
        println!("Root node type: {:?}", parsed_plan.root.node_type);
        println!("Root children count: {}", parsed_plan.root.children.len());
        println!("Total nodes: {}", parsed_plan.node_count());
        println!("Max depth: {}", parsed_plan.max_depth());

        // Should have 2 child nodes (Index Scan and Seq Scan)
        assert_eq!(
            parsed_plan.root.children.len(),
            2,
            "Root node should have 2 children"
        );
        assert!(
            parsed_plan.node_count() >= 3,
            "Should have at least 3 nodes total"
        );
        assert!(
            parsed_plan.max_depth() >= 2,
            "Should have depth of at least 2"
        );

        println!("\nTesting via QueryPlan constructor...");

        // Test via new parsing architecture
        use crate::parsing::{ParseMetadata, PlanFactory, PlanParserCore, TextPlanParser};

        let timestamp = Utc::now();
        let metadata = ParseMetadata::new(timestamp, 1423.264, "SELECT test".to_string());
        let parser = TextPlanParser::new().unwrap();
        let parsed_result = parser.parse(&plan_text, metadata).unwrap();

        let query_plan = PlanFactory::create_query_plan_from_parsed(
            timestamp,
            1423.264,
            "SELECT test".to_string(),
            plan_text.to_string(),
            parsed_result,
        )
        .unwrap();

        let parsed_via_query_plan = query_plan.parsed();
        println!("Root node type: {:?}", parsed_via_query_plan.root.node_type);
        println!(
            "Root children count: {}",
            parsed_via_query_plan.root.children.len()
        );
        println!("Total nodes: {}", parsed_via_query_plan.node_count());
        println!("Max depth: {}", parsed_via_query_plan.max_depth());

        // Should have same results
        assert_eq!(
            parsed_via_query_plan.root.children.len(),
            2,
            "QueryPlan parsing should also have 2 children"
        );
        assert!(
            parsed_via_query_plan.node_count() >= 3,
            "QueryPlan should have at least 3 nodes total"
        );
        assert!(
            parsed_via_query_plan.max_depth() >= 2,
            "QueryPlan should have depth of at least 2"
        );

        println!("\n✅ Both parsing methods work correctly!");
    }
}
