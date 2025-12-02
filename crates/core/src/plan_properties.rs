//! Typed property system for PostgreSQL execution plan nodes
//!
//! This module replaces the generic HashMap<String, String> with a strongly
//! typed property system that provides better performance and type safety.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

/// Strongly typed properties for PostgreSQL execution plan nodes
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlanProperty {
    // Output and projection properties
    Output(String),

    // Condition properties
    IndexCond(String),
    Filter(String),
    JoinFilter(String),
    RecheckCond(String),
    HashCond(String),
    MergeCond(String),

    // Sorting and grouping
    SortKey(String),
    GroupKey(String),

    // Table and index references
    RelationName(String),
    IndexName(String),
    Alias(String),

    // Caching properties
    CacheKey(String),
    CacheMode(String),

    // Parallel execution
    WorkersPlanned(u32),
    WorkersLaunched(u32),

    // Join properties
    InnerUnique(bool),

    // Sort execution details
    SortMethod(String),
    SortSpaceUsed(String),

    // Function properties (for function scans)
    Function(String),

    // Subplan properties
    SubplanName(String),

    // Performance metrics - rows removed
    RowsRemovedByFilter(u64),
    RowsRemovedByIndexRecheck(u64),
    RowsRemovedByJoinFilter(u64),

    // Bitmap scan properties
    HeapBlocksExact(u64),
    HeapBlocksLossy(u64),
    HeapFetches(u64),

    // One-time filters (for subplans)
    OneTimeFilter(String),

    // Execution metrics
    Loops(u32),
    PeakMemoryUsage(String),
    SortSpaceType(String),
    Batches(u32),

    // Custom/unknown properties (fallback)
    Custom { key: String, value: String },
}

impl PlanProperty {
    /// Get the property key as it appears in PostgreSQL output
    pub fn key(&self) -> &str {
        match self {
            PlanProperty::Output(_) => "Output",
            PlanProperty::IndexCond(_) => "Index Cond",
            PlanProperty::Filter(_) => "Filter",
            PlanProperty::JoinFilter(_) => "Join Filter",
            PlanProperty::RecheckCond(_) => "Recheck Cond",
            PlanProperty::HashCond(_) => "Hash Cond",
            PlanProperty::MergeCond(_) => "Merge Cond",
            PlanProperty::SortKey(_) => "Sort Key",
            PlanProperty::GroupKey(_) => "Group Key",
            PlanProperty::RelationName(_) => "Relation Name",
            PlanProperty::IndexName(_) => "Index Name",
            PlanProperty::Alias(_) => "Alias",
            PlanProperty::CacheKey(_) => "Cache Key",
            PlanProperty::CacheMode(_) => "Cache Mode",
            PlanProperty::WorkersPlanned(_) => "Workers Planned",
            PlanProperty::WorkersLaunched(_) => "Workers Launched",
            PlanProperty::InnerUnique(_) => "Inner Unique",
            PlanProperty::SortMethod(_) => "Sort Method",
            PlanProperty::SortSpaceUsed(_) => "Sort Space Used",
            PlanProperty::Function(_) => "Function",
            PlanProperty::SubplanName(_) => "Subplan Name",
            PlanProperty::RowsRemovedByFilter(_) => "Rows Removed by Filter",
            PlanProperty::RowsRemovedByIndexRecheck(_) => "Rows Removed by Index Recheck",
            PlanProperty::RowsRemovedByJoinFilter(_) => "Rows Removed by Join Filter",
            PlanProperty::HeapBlocksExact(_) => "Heap Blocks: exact",
            PlanProperty::HeapBlocksLossy(_) => "Heap Blocks: lossy",
            PlanProperty::HeapFetches(_) => "Heap Fetches",
            PlanProperty::OneTimeFilter(_) => "One-Time Filter",
            PlanProperty::Loops(_) => "Loops",
            PlanProperty::PeakMemoryUsage(_) => "Peak Memory Usage",
            PlanProperty::SortSpaceType(_) => "Sort Space Type",
            PlanProperty::Batches(_) => "Batches",
            PlanProperty::Custom { key, .. } => key,
        }
    }

    /// Get the property value as a Cow<str> (avoids cloning for string variants)
    ///
    /// This method returns a borrowed reference for string variants and only
    /// allocates for numeric types that need to be formatted.
    pub fn value(&self) -> Cow<'_, str> {
        match self {
            // String variants - return borrowed reference (no allocation)
            PlanProperty::Output(v) => Cow::Borrowed(v),
            PlanProperty::IndexCond(v) => Cow::Borrowed(v),
            PlanProperty::Filter(v) => Cow::Borrowed(v),
            PlanProperty::JoinFilter(v) => Cow::Borrowed(v),
            PlanProperty::RecheckCond(v) => Cow::Borrowed(v),
            PlanProperty::HashCond(v) => Cow::Borrowed(v),
            PlanProperty::MergeCond(v) => Cow::Borrowed(v),
            PlanProperty::SortKey(v) => Cow::Borrowed(v),
            PlanProperty::GroupKey(v) => Cow::Borrowed(v),
            PlanProperty::RelationName(v) => Cow::Borrowed(v),
            PlanProperty::IndexName(v) => Cow::Borrowed(v),
            PlanProperty::Alias(v) => Cow::Borrowed(v),
            PlanProperty::CacheKey(v) => Cow::Borrowed(v),
            PlanProperty::CacheMode(v) => Cow::Borrowed(v),
            PlanProperty::SortMethod(v) => Cow::Borrowed(v),
            PlanProperty::SortSpaceUsed(v) => Cow::Borrowed(v),
            PlanProperty::Function(v) => Cow::Borrowed(v),
            PlanProperty::SubplanName(v) => Cow::Borrowed(v),
            PlanProperty::OneTimeFilter(v) => Cow::Borrowed(v),
            PlanProperty::PeakMemoryUsage(v) => Cow::Borrowed(v),
            PlanProperty::SortSpaceType(v) => Cow::Borrowed(v),
            PlanProperty::Custom { value, .. } => Cow::Borrowed(value),

            // Numeric variants - must allocate to format
            PlanProperty::WorkersPlanned(v) => Cow::Owned(v.to_string()),
            PlanProperty::WorkersLaunched(v) => Cow::Owned(v.to_string()),
            PlanProperty::InnerUnique(v) => Cow::Owned(v.to_string()),
            PlanProperty::RowsRemovedByFilter(v) => Cow::Owned(v.to_string()),
            PlanProperty::RowsRemovedByIndexRecheck(v) => Cow::Owned(v.to_string()),
            PlanProperty::RowsRemovedByJoinFilter(v) => Cow::Owned(v.to_string()),
            PlanProperty::HeapBlocksExact(v) => Cow::Owned(v.to_string()),
            PlanProperty::HeapBlocksLossy(v) => Cow::Owned(v.to_string()),
            PlanProperty::HeapFetches(v) => Cow::Owned(v.to_string()),
            PlanProperty::Loops(v) => Cow::Owned(v.to_string()),
            PlanProperty::Batches(v) => Cow::Owned(v.to_string()),
        }
    }

    /// Parse a string key-value pair into a typed property
    pub fn from_key_value(key: &str, value: &str) -> Self {
        match key {
            "Output" => PlanProperty::Output(value.to_string()),
            "Index Cond" => PlanProperty::IndexCond(value.to_string()),
            "Filter" => PlanProperty::Filter(value.to_string()),
            "Join Filter" => PlanProperty::JoinFilter(value.to_string()),
            "Recheck Cond" => PlanProperty::RecheckCond(value.to_string()),
            "Hash Cond" => PlanProperty::HashCond(value.to_string()),
            "Merge Cond" => PlanProperty::MergeCond(value.to_string()),
            "Sort Key" => PlanProperty::SortKey(value.to_string()),
            "Group Key" => PlanProperty::GroupKey(value.to_string()),
            "Relation Name" => PlanProperty::RelationName(value.to_string()),
            "Index Name" => PlanProperty::IndexName(value.to_string()),
            "Alias" => PlanProperty::Alias(value.to_string()),
            "Cache Key" => PlanProperty::CacheKey(value.to_string()),
            "Cache Mode" => PlanProperty::CacheMode(value.to_string()),
            "Workers Planned" => {
                if let Ok(workers) = value.parse::<u32>() {
                    PlanProperty::WorkersPlanned(workers)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Workers Launched" => {
                if let Ok(workers) = value.parse::<u32>() {
                    PlanProperty::WorkersLaunched(workers)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Inner Unique" => match value.to_lowercase().as_str() {
                "true" | "t" | "yes" | "1" => PlanProperty::InnerUnique(true),
                "false" | "f" | "no" | "0" => PlanProperty::InnerUnique(false),
                _ => PlanProperty::Custom {
                    key: key.to_string(),
                    value: value.to_string(),
                },
            },
            "Sort Method" => PlanProperty::SortMethod(value.to_string()),
            "Sort Space Used" => PlanProperty::SortSpaceUsed(value.to_string()),
            "Function" => PlanProperty::Function(value.to_string()),
            "Subplan Name" => PlanProperty::SubplanName(value.to_string()),
            "Rows Removed by Filter" => {
                if let Ok(rows) = value.parse::<u64>() {
                    PlanProperty::RowsRemovedByFilter(rows)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Rows Removed by Index Recheck" => {
                if let Ok(rows) = value.parse::<u64>() {
                    PlanProperty::RowsRemovedByIndexRecheck(rows)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Rows Removed by Join Filter" => {
                if let Ok(rows) = value.parse::<u64>() {
                    PlanProperty::RowsRemovedByJoinFilter(rows)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Heap Blocks: exact" => {
                if let Ok(blocks) = value.parse::<u64>() {
                    PlanProperty::HeapBlocksExact(blocks)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Heap Blocks: lossy" => {
                if let Ok(blocks) = value.parse::<u64>() {
                    PlanProperty::HeapBlocksLossy(blocks)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Heap Fetches" => {
                if let Ok(fetches) = value.parse::<u64>() {
                    PlanProperty::HeapFetches(fetches)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "One-Time Filter" => PlanProperty::OneTimeFilter(value.to_string()),
            "Loops" => {
                if let Ok(loops) = value.parse::<u32>() {
                    PlanProperty::Loops(loops)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            "Peak Memory Usage" => PlanProperty::PeakMemoryUsage(value.to_string()),
            "Sort Space Type" => PlanProperty::SortSpaceType(value.to_string()),
            "Batches" => {
                if let Ok(batches) = value.parse::<u32>() {
                    PlanProperty::Batches(batches)
                } else {
                    PlanProperty::Custom {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
            }
            _ => PlanProperty::Custom {
                key: key.to_string(),
                value: value.to_string(),
            },
        }
    }
}

/// Typed property collection that replaces HashMap<String, String>
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanProperties {
    properties: Vec<PlanProperty>,
}

impl PlanProperties {
    /// Create a new empty property collection
    pub fn new() -> Self {
        Self {
            properties: Vec::new(),
        }
    }

    /// Set a property by key-value pair (for backward compatibility)
    pub fn set(&mut self, key: &str, value: &str) {
        let property = PlanProperty::from_key_value(key, value);

        // Remove existing property with the same key
        self.properties.retain(|p| p.key() != key);

        // Add the new property
        self.properties.push(property);
    }

    /// Set a typed property directly
    pub fn set_property(&mut self, property: PlanProperty) {
        let key = property.key();

        // Remove existing property with the same key
        self.properties.retain(|p| p.key() != key);

        // Add the new property
        self.properties.push(property);
    }

    /// Get a property value by key (for backward compatibility)
    /// Note: This allocates a new String. Use get_property() for zero-copy access.
    pub fn get(&self, key: &str) -> Option<String> {
        self.properties
            .iter()
            .find(|p| p.key() == key)
            .map(|p| p.value().into_owned())
    }

    /// Get a typed property by key
    pub fn get_property(&self, key: &str) -> Option<&PlanProperty> {
        self.properties.iter().find(|p| p.key() == key)
    }

    /// Get all properties as an iterator
    pub fn iter(&self) -> impl Iterator<Item = &PlanProperty> {
        self.properties.iter()
    }

    /// Convert to HashMap for backward compatibility
    /// Note: This allocates new Strings. Use iter() for zero-copy access.
    pub fn to_hashmap(&self) -> HashMap<String, String> {
        self.properties
            .iter()
            .map(|p| (p.key().to_string(), p.value().into_owned()))
            .collect()
    }

    /// Create from HashMap for migration
    pub fn from_hashmap(map: &HashMap<String, String>) -> Self {
        let mut properties = PlanProperties::new();
        for (key, value) in map {
            properties.set(key, value);
        }
        properties
    }

    /// Check if properties is empty
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Get the number of properties
    pub fn len(&self) -> usize {
        self.properties.len()
    }

    // Strongly typed accessors for common properties

    pub fn output(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::Output(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn index_condition(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::IndexCond(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn filter(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::Filter(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn join_filter(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::JoinFilter(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn sort_key(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::SortKey(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn group_key(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::GroupKey(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn relation_name(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::RelationName(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn index_name(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::IndexName(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn workers_planned(&self) -> Option<u32> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::WorkersPlanned(v) => Some(*v),
            _ => None,
        })
    }

    pub fn workers_launched(&self) -> Option<u32> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::WorkersLaunched(v) => Some(*v),
            _ => None,
        })
    }

    pub fn inner_unique(&self) -> Option<bool> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::InnerUnique(v) => Some(*v),
            _ => None,
        })
    }

    pub fn rows_removed_by_filter(&self) -> Option<u64> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::RowsRemovedByFilter(v) => Some(*v),
            _ => None,
        })
    }

    pub fn rows_removed_by_index_recheck(&self) -> Option<u64> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::RowsRemovedByIndexRecheck(v) => Some(*v),
            _ => None,
        })
    }

    pub fn rows_removed_by_join_filter(&self) -> Option<u64> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::RowsRemovedByJoinFilter(v) => Some(*v),
            _ => None,
        })
    }

    pub fn heap_blocks_exact(&self) -> Option<u64> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::HeapBlocksExact(v) => Some(*v),
            _ => None,
        })
    }

    pub fn heap_blocks_lossy(&self) -> Option<u64> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::HeapBlocksLossy(v) => Some(*v),
            _ => None,
        })
    }

    pub fn heap_fetches(&self) -> Option<u64> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::HeapFetches(v) => Some(*v),
            _ => None,
        })
    }

    pub fn one_time_filter(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::OneTimeFilter(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn loops(&self) -> Option<u32> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::Loops(v) => Some(*v),
            _ => None,
        })
    }

    pub fn peak_memory_usage(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::PeakMemoryUsage(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn sort_space_type(&self) -> Option<&str> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::SortSpaceType(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn batches(&self) -> Option<u32> {
        self.properties.iter().find_map(|p| match p {
            PlanProperty::Batches(v) => Some(*v),
            _ => None,
        })
    }
}

impl Default for PlanProperties {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_property_creation_and_access() {
        let mut props = PlanProperties::new();

        // Test string properties
        props.set("Output", "id, name");
        props.set("Filter", "(active = true)");

        assert_eq!(props.get("Output"), Some("id, name".to_string()));
        assert_eq!(props.get("Filter"), Some("(active = true)".to_string()));
        assert_eq!(props.get("Nonexistent"), None);
    }

    #[test]
    fn test_typed_property_creation() {
        let mut props = PlanProperties::new();

        props.set_property(PlanProperty::WorkersPlanned(4));
        props.set_property(PlanProperty::InnerUnique(true));

        assert_eq!(props.workers_planned(), Some(4));
        assert_eq!(props.inner_unique(), Some(true));
        assert_eq!(props.get("Workers Planned"), Some("4".to_string()));
        assert_eq!(props.get("Inner Unique"), Some("true".to_string()));
    }

    #[test]
    fn test_property_parsing() {
        let mut props = PlanProperties::new();

        // Test numeric parsing
        props.set("Workers Planned", "8");
        assert_eq!(props.workers_planned(), Some(8));

        // Test boolean parsing
        props.set("Inner Unique", "true");
        assert_eq!(props.inner_unique(), Some(true));

        props.set("Inner Unique", "false");
        assert_eq!(props.inner_unique(), Some(false));

        // Test invalid numeric (should fall back to custom)
        props.set("Workers Planned", "invalid");
        assert_eq!(props.workers_planned(), None);
        assert_eq!(props.get("Workers Planned"), Some("invalid".to_string()));
    }

    #[test]
    fn test_hashmap_compatibility() {
        let mut map = HashMap::new();
        map.insert("Output".to_string(), "id, name".to_string());
        map.insert("Workers Planned".to_string(), "4".to_string());

        let props = PlanProperties::from_hashmap(&map);

        assert_eq!(props.output(), Some("id, name"));
        assert_eq!(props.workers_planned(), Some(4));

        let back_to_map = props.to_hashmap();
        assert_eq!(back_to_map.get("Output"), Some(&"id, name".to_string()));
        assert_eq!(back_to_map.get("Workers Planned"), Some(&"4".to_string()));
    }

    #[test]
    fn test_property_replacement() {
        let mut props = PlanProperties::new();

        props.set("Output", "id");
        props.set("Output", "id, name"); // Should replace

        assert_eq!(props.len(), 1);
        assert_eq!(props.output(), Some("id, name"));
    }

    #[test]
    fn test_custom_properties() {
        let mut props = PlanProperties::new();

        props.set("Custom Property", "custom value");

        assert_eq!(
            props.get("Custom Property"),
            Some("custom value".to_string())
        );

        if let Some(PlanProperty::Custom { key, value }) = props.get_property("Custom Property") {
            assert_eq!(key, "Custom Property");
            assert_eq!(value, "custom value");
        } else {
            panic!("Expected custom property");
        }
    }
}
