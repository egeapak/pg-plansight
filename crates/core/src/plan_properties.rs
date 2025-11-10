//! Typed property system for PostgreSQL execution plan nodes
//!
//! This module replaces the generic HashMap<String, String> with a strongly
//! typed property system that provides better performance and type safety.

use serde::{Deserialize, Serialize};
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
            PlanProperty::Custom { key, .. } => key,
        }
    }

    /// Get the property value as a string (for backward compatibility)
    pub fn value(&self) -> String {
        match self {
            PlanProperty::Output(v) => v.clone(),
            PlanProperty::IndexCond(v) => v.clone(),
            PlanProperty::Filter(v) => v.clone(),
            PlanProperty::JoinFilter(v) => v.clone(),
            PlanProperty::RecheckCond(v) => v.clone(),
            PlanProperty::HashCond(v) => v.clone(),
            PlanProperty::MergeCond(v) => v.clone(),
            PlanProperty::SortKey(v) => v.clone(),
            PlanProperty::GroupKey(v) => v.clone(),
            PlanProperty::RelationName(v) => v.clone(),
            PlanProperty::IndexName(v) => v.clone(),
            PlanProperty::Alias(v) => v.clone(),
            PlanProperty::CacheKey(v) => v.clone(),
            PlanProperty::CacheMode(v) => v.clone(),
            PlanProperty::WorkersPlanned(v) => v.to_string(),
            PlanProperty::WorkersLaunched(v) => v.to_string(),
            PlanProperty::InnerUnique(v) => v.to_string(),
            PlanProperty::SortMethod(v) => v.clone(),
            PlanProperty::SortSpaceUsed(v) => v.clone(),
            PlanProperty::Function(v) => v.clone(),
            PlanProperty::SubplanName(v) => v.clone(),
            PlanProperty::Custom { value, .. } => value.clone(),
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
    pub fn get(&self, key: &str) -> Option<String> {
        self.properties
            .iter()
            .find(|p| p.key() == key)
            .map(|p| p.value())
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
    pub fn to_hashmap(&self) -> HashMap<String, String> {
        self.properties
            .iter()
            .map(|p| (p.key().to_string(), p.value()))
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
