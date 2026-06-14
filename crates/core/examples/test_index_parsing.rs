use pg_plansight_core::plan_parser::ScanType;

fn main() {
    let test_line =
        r#"Index Scan using "IX_BloodGasDevices_AcceptanceId" on "Shared"."BloodGasDevices" b"#;

    println!("Testing line: {}", test_line);

    match ScanType::analyze(test_line) {
        Ok(scan_type) => {
            println!("Parsed scan type: {:?}", scan_type);

            match scan_type {
                ScanType::IndexScan {
                    table,
                    index,
                    backward,
                    only,
                } => {
                    println!("Table: {:?}", table);
                    println!("Index: {:?}", index);
                    println!("Backward: {}", backward);
                    println!("Only: {}", only);
                }
                _ => println!("Not an IndexScan type"),
            }
        }
        Err(e) => {
            println!("Parsing failed: {:?}", e);
        }
    }

    // Test the second child node
    let test_line2 = r#"Index Only Scan using "PK_Acceptances" on "Shared"."Acceptances" a"#;

    println!("\nTesting line 2: {}", test_line2);

    match ScanType::analyze(test_line2) {
        Ok(scan_type) => {
            println!("Parsed scan type: {:?}", scan_type);

            match scan_type {
                ScanType::IndexScan {
                    table,
                    index,
                    backward,
                    only,
                } => {
                    println!("Table: {:?}", table);
                    println!("Index: {:?}", index);
                    println!("Backward: {}", backward);
                    println!("Only: {}", only);
                }
                _ => println!("Not an IndexScan type"),
            }
        }
        Err(e) => {
            println!("Parsing failed: {:?}", e);
        }
    }
}
