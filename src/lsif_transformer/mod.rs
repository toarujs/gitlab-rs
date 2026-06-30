#![allow(dead_code, unused_imports)]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LSIFEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Documentation {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LSIFTransformer {
    pub max_entries: usize,
    pub max_docs: usize,
}

impl LSIFTransformer {
    pub fn new() -> Self {
        Self {
            max_entries: 10000,
            max_docs: 1000,
        }
    }

    pub fn with_limits(max_entries: usize, max_docs: usize) -> Self {
        Self {
            max_entries,
            max_docs,
        }
    }

    pub fn validate_entry(&self, entry: &LSIFEntry) -> bool {
        !entry.id.is_empty() && !entry.entry_type.is_empty()
    }

    pub fn transform_entries<'a>(&self, entries: &'a [LSIFEntry]) -> Vec<&'a LSIFEntry> {
        entries
            .iter()
            .filter(|e| self.validate_entry(e))
            .take(self.max_entries)
            .collect()
    }
}

impl Default for LSIFTransformer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_entry() {
        let transformer = LSIFTransformer::new();
        let valid = LSIFEntry {
            id: "1".to_string(),
            entry_type: "vertex".to_string(),
            label: "test".to_string(),
            uri: String::new(),
        };
        assert!(transformer.validate_entry(&valid));

        let invalid = LSIFEntry {
            id: "".to_string(),
            entry_type: "vertex".to_string(),
            label: String::new(),
            uri: String::new(),
        };
        assert!(!transformer.validate_entry(&invalid));
    }

    #[test]
    fn test_transform_entries_limit() {
        let transformer = LSIFTransformer::with_limits(2, 100);
        let entries = vec![
            LSIFEntry { id: "1".to_string(), entry_type: "v".to_string(), label: String::new(), uri: String::new() },
            LSIFEntry { id: "2".to_string(), entry_type: "v".to_string(), label: String::new(), uri: String::new() },
            LSIFEntry { id: "3".to_string(), entry_type: "v".to_string(), label: String::new(), uri: String::new() },
        ];
        assert_eq!(transformer.transform_entries(&entries).len(), 2);
    }
}
