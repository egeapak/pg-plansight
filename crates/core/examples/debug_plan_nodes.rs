use pg_loganalyze_core::PlanParser;

fn main() {
    let plan_text = "    Nested Loop  (cost=0.71..31837.72 rows=837 width=72)
      Output: b.\"Id\", b.\"AcceptanceId\", b.\"CreatedDate\", b.\"DeviceName\", b.\"IsActive\", b.\"MeasuredDate\", b.\"SampleType\", b.\"TestId\"
      Inner Unique: true
      ->  Index Scan using \"IX_BloodGasDevices_AcceptanceId\" on \"Shared\".\"BloodGasDevices\" b  (cost=0.43..31568.85 rows=837 width=72)
            Output: b.\"Id\", b.\"AcceptanceId\", b.\"CreatedDate\", b.\"DeviceName\", b.\"IsActive\", b.\"MeasuredDate\", b.\"SampleType\", b.\"TestId\"
            Index Cond: (b.\"AcceptanceId\" = ANY ('{958,963,968,969,971,975,990,1198,1171,1157,1172,1178,959,962,977,973,1160,966,1222,1280,363}'::integer[]))
            Filter: (b.\"IsActive\" AND (b.\"MeasuredDate\" >= '2025-06-14 09:02:49.528988+00'::timestamp with time zone))
      ->  Index Only Scan using \"PK_Acceptances\" on \"Shared\".\"Acceptances\" a  (cost=0.28..0.32 rows=1 width=4)
            Output: a.\"Id\"
            Index Cond: (a.\"Id\" = b.\"AcceptanceId\")";

    let parser = PlanParser::new().expect("Failed to create PlanParser");
    match parser.parse_plan(plan_text) {
        Ok(plan) => {
            println!("\n=== PLAN DEBUG ANALYSIS ===");
            println!("SQL Query:");
            println!("SELECT");
            println!("  b.\"Id\", b.\"AcceptanceId\", b.\"CreatedDate\", b.\"DeviceName\",");
            println!("  b.\"IsActive\", b.\"MeasuredDate\", b.\"SampleType\", b.\"TestId\"");
            println!("FROM");
            println!("  \"Shared\".\"BloodGasDevices\" AS b");
            println!(
                "  INNER JOIN \"Shared\".\"Acceptances\" AS a ON b.\"AcceptanceId\" = a.\"Id\""
            );
            println!("WHERE");
            println!(
                "  b.\"IsActive\" AND b.\"AcceptanceId\" = ANY($1) AND b.\"MeasuredDate\" >= $2"
            );
            println!();

            debug_plan_node(&plan.root, 0);
        }
        Err(e) => {
            println!("Failed to parse plan: {:?}", e);
        }
    }
}

fn debug_plan_node(node: &pg_loganalyze_core::PlanNode, indent: usize) {
    let indent_str = "  ".repeat(indent);

    println!("{}=== NODE {} ===", indent_str, indent);
    println!("{}NodeType: {:?}", indent_str, node.node_type);
    println!("{}Description: \"{}\"", indent_str, node.description());
    println!(
        "{}Cost: startup={:.2}, total={:.2}..{:.2}",
        indent_str, node.cost.startup_cost, node.cost.min_total_cost, node.cost.max_total_cost
    );
    println!(
        "{}Rows: estimated={}, width={}",
        indent_str, node.cost.estimated_rows, node.cost.estimated_width
    );

    // Extract table and index names using the methods
    let table_name = node.extract_table_name();
    let index_name = node.extract_index_name();
    println!("{}Table: \"{}\"", indent_str, table_name);
    println!("{}Index: \"{}\"", indent_str, index_name);

    // Show properties using the correct API
    let properties = node.properties();
    if !properties.is_empty() {
        println!("{}Properties:", indent_str);
        for property in properties.iter() {
            match property {
                pg_loganalyze_core::PlanProperty::Custom { key, value } => {
                    println!("{}  {}: {}", indent_str, key, value);
                }
                pg_loganalyze_core::PlanProperty::Output(output) => {
                    println!("{}  Output: {}", indent_str, output);
                }
                pg_loganalyze_core::PlanProperty::IndexCond(cond) => {
                    println!("{}  Index Cond: {}", indent_str, cond);
                }
                pg_loganalyze_core::PlanProperty::Filter(filter) => {
                    println!("{}  Filter: {}", indent_str, filter);
                }
                pg_loganalyze_core::PlanProperty::JoinFilter(filter) => {
                    println!("{}  Join Filter: {}", indent_str, filter);
                }
                pg_loganalyze_core::PlanProperty::SortKey(key) => {
                    println!("{}  Sort Key: {}", indent_str, key);
                }
                pg_loganalyze_core::PlanProperty::GroupKey(key) => {
                    println!("{}  Group Key: {}", indent_str, key);
                }
                pg_loganalyze_core::PlanProperty::RelationName(name) => {
                    println!("{}  Relation Name: {}", indent_str, name);
                }
                pg_loganalyze_core::PlanProperty::IndexName(name) => {
                    println!("{}  Index Name: {}", indent_str, name);
                }
                pg_loganalyze_core::PlanProperty::WorkersPlanned(count) => {
                    println!("{}  Workers Planned: {}", indent_str, count);
                }
                pg_loganalyze_core::PlanProperty::WorkersLaunched(count) => {
                    println!("{}  Workers Launched: {}", indent_str, count);
                }
                pg_loganalyze_core::PlanProperty::InnerUnique(unique) => {
                    println!("{}  Inner Unique: {}", indent_str, unique);
                }
                _ => {
                    println!("{}  Other Property: {:?}", indent_str, property);
                }
            }
        }
    }

    // Show actuals if available
    if let Some(actuals) = &node.actuals {
        println!("{}Actuals:", indent_str);
        if let Some(time) = actuals.actual_time_ms {
            println!("{}  actual_time_ms: {:.2}", indent_str, time);
        }
        if let Some(rows) = actuals.actual_rows {
            println!("{}  actual_rows: {}", indent_str, rows);
        }
        if let Some(loops) = actuals.actual_loops {
            println!("{}  actual_loops: {}", indent_str, loops);
        }
    }

    println!();

    // Recursively debug children
    for (i, child) in node.children.iter().enumerate() {
        println!("{}Child {} of {}:", indent_str, i + 1, node.children.len());
        debug_plan_node(child, indent + 1);
    }
}
