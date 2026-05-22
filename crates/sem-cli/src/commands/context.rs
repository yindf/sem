use std::path::Path;

use colored::Colorize;
use sem_core::parser::context::build_context;

pub struct ContextOptions {
    pub cwd: String,
    pub entity_name: Option<String>,
    pub entity_id: Option<String>,
    pub file_path: Option<String>,
    pub budget: usize,
    pub json: bool,
    pub file_exts: Vec<String>,
    pub no_cache: bool,
}

pub fn context_command(opts: ContextOptions) {
    let root = Path::new(&opts.cwd);
    let registry = super::create_registry(&opts.cwd);
    let ext_filter = super::graph::normalize_exts(&opts.file_exts);

    let file_paths = super::graph::find_supported_files_public(root, &registry, &ext_filter);
    let (graph, all_entities) = super::graph::get_or_build_graph(root, &file_paths, &registry, opts.no_cache);

    let entity = super::find_entity_in_graph(&graph, &all_entities, opts.entity_name.as_deref(), opts.entity_id.as_deref(), opts.file_path.as_deref(), "sem context");
    let entries = build_context(&graph, &entity.id, &all_entities, opts.budget);

    let total_tokens: usize = entries.iter().map(|e| e.estimated_tokens).sum();

    if opts.json {
        let output = serde_json::json!({
            "entity": entity.name,
            "entityId": entity.id,
            "budget": opts.budget,
            "total_tokens": total_tokens,
            "entries": entries.iter().map(|e| serde_json::json!({
                "entityId": e.entity_id,
                "name": e.entity_name,
                "type": e.entity_type,
                "file": e.file_path,
                "role": e.role,
                "tokens": e.estimated_tokens,
                "content": e.content,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&output).unwrap());
    } else {
        println!(
            "{} {} {} (budget: {}, used: {})\n",
            "context for".green().bold(),
            entity.entity_type.dimmed(),
            entity.name.bold(),
            opts.budget,
            total_tokens,
        );

        let mut current_role = String::new();
        for entry in &entries {
            if entry.role != current_role {
                current_role = entry.role.clone();
                let role_label = match current_role.as_str() {
                    "target" => "target".green().bold(),
                    "direct_dependent" => "direct dependents".yellow().bold(),
                    "transitive_dependent" => "transitive dependents".dimmed().bold(),
                    _ => current_role.normal().bold(),
                };
                println!("  {}:", role_label);
            }

            let snippet: String = entry.content.lines().next().unwrap_or("").to_string();
            println!(
                "    {} {} ({}, ~{} tokens)",
                entry.entity_type.dimmed(),
                entry.entity_name.bold(),
                entry.file_path.dimmed(),
                entry.estimated_tokens,
            );
            if !snippet.is_empty() {
                println!("      {}", snippet.dimmed());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sem_core::model::entity::SemanticEntity;
    use sem_core::parser::graph::{EntityGraph, EntityInfo};
    use std::collections::HashMap;
    use super::super::find_entity_in_graph;

    fn make_entity(name: &str, signature: Option<&str>, line: usize) -> SemanticEntity {
        let sig_str = signature.unwrap_or("");
        SemanticEntity {
            id: format!("svc.cs::method::{name}{sig_str}"),
            file_path: "svc.cs".to_string(),
            entity_type: "method".to_string(),
            name: name.to_string(),
            signature: signature.map(|s| s.to_string()),
            parent_id: None,
            content: String::new(),
            content_hash: format!("hash_{name}_{line}"),
            structural_hash: None,
            start_line: line,
            end_line: line + 10,
            metadata: None,
        }
    }

    fn build_graph(entities: &[SemanticEntity]) -> (EntityGraph, Vec<SemanticEntity>) {
        let graph_entities: HashMap<String, EntityInfo> = entities.iter().map(|e| {
            (e.id.clone(), EntityInfo {
                id: e.id.clone(),
                name: e.name.clone(),
                entity_type: e.entity_type.clone(),
                file_path: e.file_path.clone(),
                parent_id: e.parent_id.clone(),
                start_line: e.start_line,
                end_line: e.end_line,
            })
        }).collect();
        (EntityGraph::from_parts(graph_entities, vec![]), entities.to_vec())
    }

    #[test]
    fn test_find_entity_by_signature_picks_correct_overload() {
        let entities = vec![
            make_entity("Process", Some("(int)"), 10),
            make_entity("Process", Some("(string)"), 30),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("Process(int)"), None, None, "sem context");
        assert_eq!(result.id, "svc.cs::method::Process(int)");
        assert_eq!(result.start_line, 10);
    }

    #[test]
    fn test_find_entity_by_different_signature() {
        let entities = vec![
            make_entity("Process", Some("(int)"), 10),
            make_entity("Process", Some("(string)"), 30),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("Process(string)"), None, None, "sem context");
        assert_eq!(result.id, "svc.cs::method::Process(string)");
        assert_eq!(result.start_line, 30);
    }

    #[test]
    fn test_find_entity_single_no_signature_needed() {
        let entities = vec![
            make_entity("Handle", None, 50),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("Handle"), None, None, "sem context");
        assert_eq!(result.name, "Handle");
    }

    #[test]
    fn test_find_entity_by_id_bypasses_name() {
        let entities = vec![
            make_entity("Process", Some("(int)"), 10),
            make_entity("Process", Some("(string)"), 30),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, None, Some("svc.cs::method::Process(string)"), None, "sem context");
        assert_eq!(result.id, "svc.cs::method::Process(string)");
    }

    #[test]
    fn test_find_entity_single_overload_no_signature_ok() {
        let entities = vec![
            make_entity("Process", Some("(int)"), 10),
            make_entity("Handle", None, 50),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("Process"), None, None, "sem context");
        assert_eq!(result.id, "svc.cs::method::Process(int)");
    }

    #[test]
    fn test_find_entity_empty_params_matches_no_signature() {
        let entities = vec![
            make_entity("ResumeAllDown", Some("()"), 199),
            make_entity("ResumeAllDown", Some("(bool)"), 467),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("ResumeAllDown()"), None, None, "sem context");
        assert_eq!(result.id, "svc.cs::method::ResumeAllDown()");
        assert_eq!(result.start_line, 199);
    }

    #[test]
    fn test_find_entity_nonempty_signature_still_works() {
        let entities = vec![
            make_entity("ResumeAllDown", Some("()"), 199),
            make_entity("ResumeAllDown", Some("(bool)"), 467),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("ResumeAllDown(bool)"), None, None, "sem context");
        assert_eq!(result.id, "svc.cs::method::ResumeAllDown(bool)");
        assert_eq!(result.start_line, 467);
    }
}
