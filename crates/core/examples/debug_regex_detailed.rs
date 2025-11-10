use regex::Regex;

fn main() {
    let current_regex = Regex::new(r#"(?<type>Bitmap)?\s*Index\s*(?<only>Only)?\s+Scan\s*(?<backward>Backward)?(?:\s+using\s+(?<index>[^\s]+))?"#).unwrap();

    let test_cases = [
        r#"Index Scan using "IX_BloodGasDevices_AcceptanceId" on "Shared"."BloodGasDevices" b"#,
        r#"Index Only Scan using "PK_Acceptances" on "Shared"."Acceptances" a"#,
        r#"Index Scan using IX_BloodGasDevices_AcceptanceId"#,
        r#"Index Only Scan using PK_Acceptances"#,
    ];

    for (i, test_line) in test_cases.iter().enumerate() {
        println!("=== Test Case {} ===", i + 1);
        println!("Line: {}", test_line);

        if let Some(captures) = current_regex.captures(test_line) {
            println!("✓ Match found!");

            // Print all groups
            for (j, group) in captures.iter().enumerate() {
                if let Some(matched) = group {
                    println!("  Group {}: '{}'", j, matched.as_str());
                }
            }

            // Print named groups
            println!("Named groups:");
            if let Some(type_match) = captures.name("type") {
                println!("  type: '{}'", type_match.as_str());
            }
            if let Some(only_match) = captures.name("only") {
                println!("  only: '{}'", only_match.as_str());
            }
            if let Some(backward_match) = captures.name("backward") {
                println!("  backward: '{}'", backward_match.as_str());
            }
            if let Some(index_match) = captures.name("index") {
                println!("  index: '{}'", index_match.as_str());
            } else {
                println!("  index: NOT CAPTURED");
            }
        } else {
            println!("✗ No match found");
        }
        println!();
    }

    // Let's try a simpler, more permissive regex
    println!("=== Testing improved regex ===");
    let improved_regex = Regex::new(r#"(?<type>Bitmap)?\s*Index\s*(?<only>Only)?\s+Scan(?:\s+(?<backward>Backward))?(?:\s+using\s+(?<index>\S+))?"#).unwrap();

    for (i, test_line) in test_cases.iter().enumerate() {
        println!("Test Case {}: {}", i + 1, test_line);

        if let Some(captures) = improved_regex.captures(test_line) {
            println!("✓ Match found!");
            if let Some(index_match) = captures.name("index") {
                println!("  index: '{}'", index_match.as_str());
            } else {
                println!("  index: NOT CAPTURED");
            }
            if let Some(only_match) = captures.name("only") {
                println!("  only: '{}'", only_match.as_str());
            }
        } else {
            println!("✗ No match found");
        }
        println!();
    }
}
