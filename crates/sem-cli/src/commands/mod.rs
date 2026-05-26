pub mod blame;
pub mod context;
pub mod diff;
pub mod entities;
pub mod graph;
pub mod impact;
pub mod log;
pub mod setup;
pub mod stats;
pub mod verify;

use colored::Colorize;
use sem_core::model::entity::SemanticEntity;
use sem_core::model::identity::parent_name;
use sem_core::parser::graph::EntityGraph;
use sem_core::parser::plugins::create_default_registry;
use sem_core::parser::registry::ParserRegistry;
use std::collections::HashMap;
use std::path::Path;

/// Create a parser registry with extension mappings loaded from `cwd`.
/// Loads `.semrc` first (takes priority), then `.gitattributes` as fallback.
pub fn create_registry(cwd: &str) -> ParserRegistry {
    let mut registry = create_default_registry();
    let root = Path::new(cwd);
    registry.load_semrc(root);
    registry.load_gitattributes(root);
    registry
}

/// Parsed entity query: "Name", "Type.Name", "Name(Signature)", "Type.Name(Signature)".
pub struct EntityQuery {
    pub name: String,
    pub signature: Option<String>,
    pub scope: Option<Vec<String>>,
}

/// Parse a potentially scope-qualified name into (scope_parts, entity_name).
/// Uses `.` as separator throughout (e.g. "Ns.Type.Method").
/// "FakeTourneyService.Method" -> (Some(["FakeTourneyService"]), "Method")
/// "Method" -> (None, "Method")
fn parse_scope_and_name(input: &str) -> (Option<Vec<String>>, String) {
    if !input.contains('.') {
        return (None, input.to_string());
    }

    let parts: Vec<&str> = input.split('.').collect();

    if parts.len() < 2 {
        return (None, input.to_string());
    }

    let name = parts.last().unwrap().to_string();
    let scope: Vec<String> = parts[..parts.len() - 1].iter().map(|s| s.to_string()).collect();
    (Some(scope), name)
}

pub fn parse_entity_query(input: &str) -> EntityQuery {
    // Step 1: Extract signature (unchanged logic)
    let (name_part, signature) = if let Some(open) = input.rfind('(') {
        if input.ends_with(')') && open > 0 {
            (input[..open].to_string(), Some(input[open..].to_string()))
        } else {
            (input.to_string(), None)
        }
    } else {
        (input.to_string(), None)
    };

    // Step 2: Parse scope from the name portion
    let (scope, name) = parse_scope_and_name(&name_part);

    EntityQuery { name, signature, scope }
}

/// Check if an entity's parent scope matches the given scope parts (suffix-based).
/// scope_parts like ["FakeTourneyService"] matches entities whose parent_name
/// is "Internal::FakeTourneyService" or just "FakeTourneyService".
fn scope_matches(entity: &SemanticEntity, scope: &[String], by_id: &HashMap<&str, &SemanticEntity>) -> bool {
    if scope.is_empty() {
        return true;
    }

    let pname = match parent_name(entity, by_id) {
        Some(name) => name,
        None => return false,
    };

    let parent_parts: Vec<&str> = pname.split('.').collect();
    if scope.len() > parent_parts.len() {
        return false;
    }

    let offset = parent_parts.len() - scope.len();
    for (i, scope_part) in scope.iter().enumerate() {
        if parent_parts[offset + i] != scope_part.as_str() {
            return false;
        }
    }
    true
}

/// Find an entity in the graph by name, ID, or name+signature.
/// Handles overload disambiguation with clear error messages.
pub fn find_entity_in_graph<'a>(
    graph: &'a EntityGraph,
    all_entities: &[SemanticEntity],
    name: Option<&str>,
    entity_id: Option<&str>,
    file_hint: Option<&str>,
    command_name: &str,
) -> &'a sem_core::parser::graph::EntityInfo {
    if let Some(id) = entity_id {
        if let Some(e) = graph.entities.get(id) {
            return e;
        }
        eprintln!("{} Entity ID '{}' not found", "error:".red().bold(), id);
        std::process::exit(1);
    }

    let name = name.unwrap_or_else(|| {
        eprintln!("{} Either entity name or --entity-id is required", "error:".red().bold());
        std::process::exit(1);
    });

    let query = parse_entity_query(name);

    let by_id: HashMap<&str, &SemanticEntity> = all_entities
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();

    let matching: Vec<&SemanticEntity> = all_entities
        .iter()
        .filter(|e| e.name == query.name)
        .filter(|e| {
            query.signature.as_ref().map_or(true, |sig| {
                e.signature.as_deref() == Some(sig.as_str())
            })
        })
        .filter(|e| {
            query.scope.as_ref().map_or(true, |scope| {
                scope_matches(e, scope, &by_id)
            })
        })
        .collect();

    if matching.is_empty() {
        // Check if name matches without scope to give a better error
        let name_only: Vec<&SemanticEntity> = all_entities
            .iter()
            .filter(|e| e.name == query.name)
            .collect();
        if !name_only.is_empty() && query.scope.is_some() {
            eprintln!(
                "{} Entity '{}' not found in scope '{}'",
                "error:".red().bold(),
                query.name,
                query.scope.as_ref().unwrap().join(".")
            );
            let unique_scopes: Vec<String> = name_only
                .iter()
                .filter_map(|e| parent_name(e, &by_id))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            if !unique_scopes.is_empty() {
                eprintln!("  Available scopes:");
                for s in &unique_scopes {
                    eprintln!("    {}.{}", s, query.name);
                }
            }
        } else {
            eprintln!("{} Entity '{}' not found", "error:".red().bold(), name);
        }
        std::process::exit(1);
    }

    if query.signature.is_none() && query.scope.is_none() && matching.len() > 1 {
        // Check if ambiguity is due to different parent types
        let unique_parents: Vec<Option<&str>> = matching
            .iter()
            .map(|e| e.parent_id.as_deref())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        if unique_parents.len() > 1 {
            // Same method name in different types -- suggest scope qualification
            eprintln!(
                "{} Entity '{}' exists in {} types:",
                "error:".red().bold(),
                query.name,
                unique_parents.len()
            );
            for e in &matching {
                let scope_label = parent_name(e, &by_id)
                    .unwrap_or_else(|| "(top-level)".to_string());
                let sig = e.signature.as_deref().unwrap_or("");
                eprintln!(
                    "  {}.{}{} (L{}:{})",
                    scope_label, e.name, sig, e.start_line, e.end_line
                );
            }
            let example_scope = parent_name(matching[0], &by_id)
                .unwrap_or_else(|| "(top-level)".to_string());
            eprintln!(
                "\nDisambiguate with scope: {} \"{}.{}\"",
                command_name, example_scope, query.name
            );
            std::process::exit(1);
        }

        // True overloads (same parent, same name, different signatures)
        eprintln!(
            "{} Entity '{}' has {} overloads:",
            "error:".red().bold(),
            query.name,
            matching.len()
        );
        for e in &matching {
            let sig = e.signature.as_deref().unwrap_or("n/a");
            eprintln!(
                "  {} {}{} (L{}:{})",
                e.entity_type, e.name, sig, e.start_line, e.end_line
            );
        }
        let example_sig = matching[0]
            .signature
            .as_deref()
            .unwrap_or("()");
        eprintln!(
            "\nSpecify the signature to disambiguate: {} \"{}{}\"",
            command_name, query.name, example_sig
        );
        std::process::exit(1);
    }

    let target = if let Some(file) = file_hint {
        matching.iter().find(|e| e.file_path == file).copied().unwrap_or(matching[0])
    } else {
        matching[0]
    };

    if let Some(e) = graph.entities.get(&target.id) {
        return e;
    }

    let mut graph_matching: Vec<_> = graph.entities.values().filter(|e| e.name == query.name).collect();
    if let Some(file) = file_hint {
        if let Some(e) = graph_matching.iter().find(|e| e.file_path == file) {
            return e;
        }
    }
    graph_matching.sort_by_key(|e| (&e.file_path, e.start_line));
    graph_matching[0]
}

/// Truncate a string to `max_chars` Unicode scalar values (codepoints), appending "..." if
/// truncated. Safe for multibyte encodings (CJK, simple emoji). Note: does not split on grapheme
/// cluster boundaries — ZWJ emoji sequences may render incorrectly at the truncation point.
///
/// If `max_chars <= 3`, no ellipsis is appended (no room); the string is simply truncated.
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars <= 3 {
        return s.chars().take(max_chars).collect();
    }
    // Use char_indices to find the byte boundary in a single pass
    let mut last_boundary = 0;
    let mut truncate_boundary = 0;
    let mut count = 0;
    for (i, c) in s.char_indices() {
        count += 1;
        if count == max_chars - 3 {
            truncate_boundary = i + c.len_utf8();
        }
        if count == max_chars {
            last_boundary = i + c.len_utf8();
            break;
        }
    }
    if count < max_chars {
        // String fits within max_chars — return as-is
        s.to_string()
    } else if s[last_boundary..].is_empty() {
        // Exactly max_chars — return as-is
        s.to_string()
    } else {
        // String exceeds max_chars — truncate with ellipsis
        format!("{}...", &s[..truncate_boundary])
    }
}

#[cfg(test)]
mod tests {
    use super::{truncate_str, parse_scope_and_name, parse_entity_query, scope_matches};

    #[test]
    fn ascii_short_string_unchanged() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn ascii_exact_length_unchanged() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn ascii_truncated_with_ellipsis() {
        // 6 chars > max 5, so take 2 chars + "..."
        assert_eq!(truncate_str("abcdef", 5), "ab...");
    }

    #[test]
    fn cjk_short_string_unchanged() {
        assert_eq!(truncate_str("日本語", 10), "日本語");
    }

    #[test]
    fn cjk_truncated_at_char_boundary() {
        // This was the original bug — byte-index slicing panicked on CJK chars.
        // "bff側でwebsocketエラーが頻発している問題を修正" is 28 chars
        let msg = "bff側でwebsocketエラーが頻発している問題を修正";
        let result = truncate_str(msg, 15);
        // 15 - 3 = 12 chars kept + "..."
        assert_eq!(result.chars().count(), 15);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn emoji_truncated_at_char_boundary() {
        let msg = "🎉🚀✨ feat: add new feature with celebration";
        let result = truncate_str(msg, 10);
        // 10 - 3 = 7 chars kept + "..."
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn mixed_cjk_ascii_truncation() {
        // Reproduces the exact scenario that caused the original panic:
        // byte-index slicing at 37 landed inside '頻' (bytes 36..39)
        let msg = ":bug: bff側でwebsocketエラーが頻発している問題を修正";
        // 35 chars, truncate at 20 to force truncation
        let result = truncate_str(msg, 20);
        assert_eq!(result.chars().count(), 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn empty_string() {
        assert_eq!(truncate_str("", 10), "");
    }

    #[test]
    fn max_chars_zero() {
        assert_eq!(truncate_str("hello", 0), "");
    }

    #[test]
    fn max_chars_one() {
        assert_eq!(truncate_str("hello", 1), "h");
    }

    #[test]
    fn max_chars_three_with_longer_string() {
        // Boundary: max_chars == 3, string is longer → no room for "...", just take 3 chars
        assert_eq!(truncate_str("hello", 3), "hel");
    }

    #[test]
    fn max_chars_four_triggers_ellipsis() {
        // max_chars == 4, string is longer → take 1 char + "..."
        assert_eq!(truncate_str("hello", 4), "h...");
    }

    // --- Scope parsing tests ---

    #[test]
    fn parse_scope_dot_separated() {
        let (scope, name) = parse_scope_and_name("FakeTourneyService.SendTourneyNewUnSignupReq");
        assert_eq!(scope, Some(vec!["FakeTourneyService".to_string()]));
        assert_eq!(name, "SendTourneyNewUnSignupReq");
    }

    #[test]
    fn parse_scope_no_scope() {
        let (scope, name) = parse_scope_and_name("Method");
        assert_eq!(scope, None);
        assert_eq!(name, "Method");
    }

    #[test]
    fn parse_scope_single_dot_is_name() {
        // A name with no dot should not be treated as scope
        let (scope, name) = parse_scope_and_name("Method");
        assert!(scope.is_none());
        assert_eq!(name, "Method");
    }

    #[test]
    fn entity_query_scope_and_signature() {
        let q = parse_entity_query("FakeTourneyService.SendTourneyNewUnSignupReq(bool,bool)");
        assert_eq!(q.name, "SendTourneyNewUnSignupReq");
        assert_eq!(q.scope, Some(vec!["FakeTourneyService".to_string()]));
        assert_eq!(q.signature.as_deref(), Some("(bool,bool)"));
    }

    #[test]
    fn entity_query_scope_only() {
        let q = parse_entity_query("FakeTourneyService.SendTourneyNewUnSignupReq");
        assert_eq!(q.name, "SendTourneyNewUnSignupReq");
        assert_eq!(q.scope, Some(vec!["FakeTourneyService".to_string()]));
        assert!(q.signature.is_none());
    }

    #[test]
    fn entity_query_backward_compat_name_only() {
        let q = parse_entity_query("SendTourneyNewUnSignupReq");
        assert_eq!(q.name, "SendTourneyNewUnSignupReq");
        assert!(q.scope.is_none());
        assert!(q.signature.is_none());
    }

    #[test]
    fn entity_query_backward_compat_name_and_sig() {
        let q = parse_entity_query("SendTourneyNewUnSignupReq(uint,uint)");
        assert_eq!(q.name, "SendTourneyNewUnSignupReq");
        assert!(q.scope.is_none());
        assert_eq!(q.signature.as_deref(), Some("(uint,uint)"));
    }

    #[test]
    fn parse_scope_dot_separated_multi_level() {
        let (scope, name) = parse_scope_and_name("jj.Core.Runtime.CoreAppStateMachine.LoadingModuleState.InitModuleAsync");
        assert_eq!(name, "InitModuleAsync");
        assert_eq!(scope, Some(vec![
            "jj".to_string(),
            "Core".to_string(),
            "Runtime".to_string(),
            "CoreAppStateMachine".to_string(),
            "LoadingModuleState".to_string(),
        ]));
    }

    #[test]
    fn parse_scope_dot_with_signature() {
        let q = parse_entity_query("jj.Core.Runtime.CoreAppStateMachine.LoadingModuleState.InitModuleAsync()");
        assert_eq!(q.name, "InitModuleAsync");
        assert_eq!(q.scope, Some(vec![
            "jj".to_string(),
            "Core".to_string(),
            "Runtime".to_string(),
            "CoreAppStateMachine".to_string(),
            "LoadingModuleState".to_string(),
        ]));
        assert_eq!(q.signature.as_deref(), Some("()"));
    }

    // --- scope_matches tests ---

    #[test]
    fn scope_matches_simple_parent() {
        use sem_core::model::entity::SemanticEntity;
        // Parent class
        let parent = SemanticEntity {
            id: "file.cs::class::FakeTourneyService".to_string(),
            file_path: "file.cs".to_string(),
            entity_type: "class".to_string(),
            name: "FakeTourneyService".to_string(),
            signature: None,
            parent_id: None,
            content: String::new(),
            content_hash: String::new(),
            structural_hash: None,
            start_line: 1,
            end_line: 10,
            metadata: None,
        };
        // Method inside the class
        let method = SemanticEntity {
            id: "file.cs::file.cs::class::FakeTourneyService::Method".to_string(),
            file_path: "file.cs".to_string(),
            entity_type: "method".to_string(),
            name: "Method".to_string(),
            signature: None,
            parent_id: Some("file.cs::class::FakeTourneyService".to_string()),
            content: String::new(),
            content_hash: String::new(),
            structural_hash: None,
            start_line: 5,
            end_line: 8,
            metadata: None,
        };

        let by_id: std::collections::HashMap<&str, &SemanticEntity> = [
            (parent.id.as_str(), &parent),
            (method.id.as_str(), &method),
        ].into_iter().collect();

        assert!(scope_matches(&method, &["FakeTourneyService".to_string()], &by_id));
        assert!(!scope_matches(&method, &["OtherClass".to_string()], &by_id));
        assert!(!scope_matches(&parent, &["FakeTourneyService".to_string()], &by_id)); // parent has no parent_id
    }

    #[test]
    fn scope_matches_nested_suffix() {
        use sem_core::model::entity::SemanticEntity;
        let ns = SemanticEntity {
            id: "file.cs::namespace::Internal".to_string(),
            file_path: "file.cs".to_string(),
            entity_type: "namespace".to_string(),
            name: "Internal".to_string(),
            signature: None,
            parent_id: None,
            content: String::new(),
            content_hash: String::new(),
            structural_hash: None,
            start_line: 1,
            end_line: 2,
            metadata: None,
        };
        let iface = SemanticEntity {
            id: "file.cs::file.cs::namespace::Internal::ITourneyServiceX".to_string(),
            file_path: "file.cs".to_string(),
            entity_type: "interface".to_string(),
            name: "ITourneyServiceX".to_string(),
            signature: None,
            parent_id: Some("file.cs::namespace::Internal".to_string()),
            content: String::new(),
            content_hash: String::new(),
            structural_hash: None,
            start_line: 3,
            end_line: 50,
            metadata: None,
        };
        let method = SemanticEntity {
            id: "file.cs::file.cs::namespace::Internal::ITourneyServiceX::Method".to_string(),
            file_path: "file.cs".to_string(),
            entity_type: "method".to_string(),
            name: "Method".to_string(),
            signature: None,
            parent_id: Some("file.cs::file.cs::namespace::Internal::ITourneyServiceX".to_string()),
            content: String::new(),
            content_hash: String::new(),
            structural_hash: None,
            start_line: 10,
            end_line: 15,
            metadata: None,
        };

        let by_id: std::collections::HashMap<&str, &SemanticEntity> = [
            (ns.id.as_str(), &ns),
            (iface.id.as_str(), &iface),
            (method.id.as_str(), &method),
        ].into_iter().collect();

        // Suffix match: just the class name
        assert!(scope_matches(&method, &["ITourneyServiceX".to_string()], &by_id));
        // Full path match
        assert!(scope_matches(&method, &["Internal".to_string(), "ITourneyServiceX".to_string()], &by_id));
        // Wrong suffix
        assert!(!scope_matches(&method, &["FakeTourneyService".to_string()], &by_id));
    }
}
