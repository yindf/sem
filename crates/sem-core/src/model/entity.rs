use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticEntity {
    pub id: String,
    pub file_path: String,
    pub entity_type: String,
    pub name: String,
    /// Parameter signature for code entities.
    /// For methods this is a normalized form like "()" or "(int,string)".
    /// None for non-code entities (classes, interfaces, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub content: String,
    pub content_hash: String,
    /// AST-based hash that strips comments and normalizes whitespace.
    /// Two entities with the same structural_hash are logically identical
    /// even if formatting/comments differ. Inspired by Unison's content-addressed model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structural_hash: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
}

/// Compute a "logical key" for an entity: (file, parent, name).
/// This key is used for Phase 1.5 matching to detect signature changes
/// on the same logical method (same name, same location, different params).
pub fn logical_key(entity: &SemanticEntity) -> String {
    match entity.parent_id.as_deref() {
        Some(pid) => format!("{}::{}::{}", entity.file_path, pid, entity.name),
        None => format!("{}::{}::{}", entity.file_path, entity.entity_type, entity.name),
    }
}

pub fn build_entity_id(
    file_path: &str,
    entity_type: &str,
    name: &str,
    signature: Option<&str>,
    parent_id: Option<&str>,
) -> String {
    let name_key = match signature {
        Some(sig) => format!("{name}{sig}"),
        None => name.to_string(),
    };
    match parent_id {
        Some(pid) => format!("{file_path}::{pid}::{name_key}"),
        None => format!("{file_path}::{entity_type}::{name_key}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_entity_id_no_parent() {
        assert_eq!(
            build_entity_id("src/main.ts", "function", "hello", None, None),
            "src/main.ts::function::hello"
        );
    }

    #[test]
    fn test_build_entity_id_with_parent() {
        let id = build_entity_id("src/main.ts", "method", "greet", None, Some("MyClass"));
        assert_eq!(id, "src/main.ts::MyClass::greet");
    }

    #[test]
    fn test_build_entity_id_with_signature() {
        let id = build_entity_id("src/main.cs", "method", "Process", Some("(int)"), Some("Calculator"));
        assert_eq!(id, "src/main.cs::Calculator::Process(int)");
    }

    #[test]
    fn test_build_entity_id_overloads_differ() {
        let id1 = build_entity_id("src/main.cs", "method", "Process", Some("(int)"), Some("Calculator"));
        let id2 = build_entity_id("src/main.cs", "method", "Process", Some("(string)"), Some("Calculator"));
        assert_ne!(id1, id2);
    }
}
