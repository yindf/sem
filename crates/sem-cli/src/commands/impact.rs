use std::path::Path;

use colored::Colorize;
use sem_core::parser::graph::EntityGraph;

pub struct ImpactOptions {
    pub cwd: String,
    pub entity_name: Option<String>,
    pub entity_id: Option<String>,
    pub file_hint: Option<String>,
    pub json: bool,
    pub file_exts: Vec<String>,
    pub mode: ImpactMode,
    pub depth: usize,
    pub no_cache: bool,
}

pub enum ImpactMode {
    All,
    Deps,
    Dependents,
    Tests,
}

pub fn impact_command(opts: ImpactOptions) {
    let root = Path::new(&opts.cwd);
    let registry = super::create_registry(&opts.cwd);

    let ext_filter = super::graph::normalize_exts(&opts.file_exts);
    let file_paths = super::graph::find_supported_files_public(root, &registry, &ext_filter);
    let (graph, all_entities) = super::graph::get_or_build_graph(root, &file_paths, &registry, opts.no_cache);

    let entity = super::find_entity_in_graph(&graph, &all_entities, opts.entity_name.as_deref(), opts.entity_id.as_deref(), opts.file_hint.as_deref(), "sem impact");

    match opts.mode {
        ImpactMode::Deps => print_deps(&graph, entity, opts.json),
        ImpactMode::Dependents => print_dependents(&graph, entity, opts.json),
        ImpactMode::Tests => print_tests(&graph, entity, &all_entities, opts.json),
        ImpactMode::All => print_all(&graph, entity, &all_entities, opts.json, opts.depth),
    }
}

fn entity_json(e: &sem_core::parser::graph::EntityInfo) -> serde_json::Value {
    serde_json::json!({
        "entityId": e.id, "name": e.name, "type": e.entity_type,
        "file": e.file_path, "lines": [e.start_line, e.end_line],
    })
}

fn entity_list_json(entities: &[&sem_core::parser::graph::EntityInfo]) -> Vec<serde_json::Value> {
    entities.iter().map(|e| entity_json(*e)).collect()
}

fn print_entity_header(e: &sem_core::parser::graph::EntityInfo) {
    println!(
        "{} {} {} ({}:{}–{})",
        "⊕".green(),
        e.entity_type.dimmed(),
        e.name.bold(),
        e.file_path.dimmed(),
        e.start_line,
        e.end_line,
    );
}

fn print_deps(graph: &EntityGraph, entity: &sem_core::parser::graph::EntityInfo, json: bool) {
    let deps = graph.get_dependencies(&entity.id);

    if json {
        let output = serde_json::json!({
            "entity": entity_json(entity),
            "dependencies": entity_list_json(&deps),
        });
        println!("{}", serde_json::to_string(&output).unwrap());
    } else {
        print_entity_header(entity);
        if deps.is_empty() {
            println!("\n  {} {}", "✓".green().bold(), "No dependencies.".dimmed());
        } else {
            println!("\n  {} {}", "→".blue(), "depends on:".dimmed());
            for dep in &deps {
                println!(
                    "    {} {} {} ({})",
                    "→".blue(),
                    dep.entity_type.dimmed(),
                    dep.name.bold(),
                    dep.file_path.dimmed(),
                );
            }
        }
        println!();
    }
}

fn print_dependents(graph: &EntityGraph, entity: &sem_core::parser::graph::EntityInfo, json: bool) {
    let dependents = graph.get_dependents(&entity.id);

    if json {
        let output = serde_json::json!({
            "entity": entity_json(entity),
            "dependents": entity_list_json(&dependents),
        });
        println!("{}", serde_json::to_string(&output).unwrap());
    } else {
        print_entity_header(entity);
        if dependents.is_empty() {
            println!("\n  {} {}", "✓".green().bold(), "No dependents.".dimmed());
        } else {
            println!("\n  {} {}", "←".yellow(), "depended on by:".dimmed());
            for dep in &dependents {
                println!(
                    "    {} {} {} ({})",
                    "←".yellow(),
                    dep.entity_type.dimmed(),
                    dep.name.bold(),
                    dep.file_path.dimmed(),
                );
            }
        }
        println!();
    }
}

fn print_tests(
    graph: &EntityGraph,
    entity: &sem_core::parser::graph::EntityInfo,
    all_entities: &[sem_core::model::entity::SemanticEntity],
    json: bool,
) {
    let tests = graph.test_impact(&entity.id, all_entities);

    if json {
        let output = serde_json::json!({
            "entity": entity_json(entity),
            "tests": entity_list_json(&tests),
        });
        println!("{}", serde_json::to_string(&output).unwrap());
    } else {
        print_entity_header(entity);
        if tests.is_empty() {
            println!("\n  {} {}", "✓".green().bold(), "No tests found.".dimmed());
        } else {
            println!(
                "\n  {} {}",
                "⚡".yellow(),
                format!("{} tests affected:", tests.len()).bold()
            );
            let mut by_file: std::collections::HashMap<&str, Vec<_>> =
                std::collections::HashMap::new();
            for t in &tests {
                by_file.entry(t.file_path.as_str()).or_default().push(t);
            }
            let mut files: Vec<_> = by_file.keys().copied().collect();
            files.sort();
            for file in files {
                println!("    {}", file.bold());
                let mut entities = by_file[file].clone();
                entities.sort_by_key(|e| e.start_line);
                for t in entities {
                    println!(
                        "      {} {} (L{}–{})",
                        t.entity_type.dimmed(),
                        t.name.bold(),
                        t.start_line,
                        t.end_line,
                    );
                }
            }
        }
        println!();
    }
}

fn print_all(
    graph: &EntityGraph,
    entity: &sem_core::parser::graph::EntityInfo,
    all_entities: &[sem_core::model::entity::SemanticEntity],
    json: bool,
    depth: usize,
) {
    let deps = graph.get_dependencies(&entity.id);
    let dependents = graph.get_dependents(&entity.id);
    let impact_bounded = graph.impact_analysis_bounded(&entity.id, depth);
    let tests = graph.test_impact(&entity.id, all_entities);

    if json {
        let impact_entities: Vec<serde_json::Value> = impact_bounded.iter().map(|(e, d)| {
            let mut v = entity_json(e);
            v.as_object_mut().unwrap().insert("depth".to_string(), serde_json::json!(d));
            v
        }).collect();
        let output = serde_json::json!({
            "entity": entity_json(entity),
            "dependencies": entity_list_json(&deps),
            "dependents": entity_list_json(&dependents),
            "impact": {
                "depth": depth,
                "total": impact_bounded.len(),
                "entities": impact_entities,
            },
            "tests": entity_list_json(&tests),
        });
        println!("{}", serde_json::to_string(&output).unwrap());
    } else {
        print_entity_header(entity);

        // Dependencies
        if !deps.is_empty() {
            println!("\n  {} {}", "→".blue(), "depends on:".dimmed());
            for dep in &deps {
                println!(
                    "    {} {} {} ({})",
                    "→".blue(),
                    dep.entity_type.dimmed(),
                    dep.name.bold(),
                    dep.file_path.dimmed(),
                );
            }
        }

        // Dependents
        if !dependents.is_empty() {
            println!("\n  {} {}", "←".yellow(), "depended on by:".dimmed());
            for dep in &dependents {
                println!(
                    "    {} {} {} ({})",
                    "←".yellow(),
                    dep.entity_type.dimmed(),
                    dep.name.bold(),
                    dep.file_path.dimmed(),
                );
            }
        }

        // Transitive impact grouped by depth
        if impact_bounded.is_empty() {
            println!(
                "\n  {} {}",
                "✓".green().bold(),
                "No other entities are affected by changes to this entity."
                    .dimmed()
            );
        } else {
            let max_depth_seen = impact_bounded.iter().map(|(_, d)| *d).max().unwrap_or(0);
            let depth_label = if depth == 0 { "unlimited".to_string() } else { format!("depth {}", depth) };
            println!(
                "\n  {} {}",
                "!".red().bold(),
                format!("{} entities transitively affected ({}):", impact_bounded.len(), depth_label).red(),
            );

            for d in 1..=max_depth_seen {
                let at_depth: Vec<_> = impact_bounded.iter().filter(|(_, dd)| *dd == d).map(|(e, _)| *e).collect();
                if at_depth.is_empty() { continue; }

                let label = if d == 1 { "Direct dependents".to_string() } else { format!("Depth {}", d) };
                println!("\n    {} ({})", label.bold(), at_depth.len());
                for imp in &at_depth {
                    println!(
                        "      {} {} {} ({}:L{})",
                        "→".red(),
                        imp.entity_type.dimmed(),
                        imp.name.bold(),
                        imp.file_path.dimmed(),
                        imp.start_line,
                    );
                }
            }
        }

        // Tests
        if !tests.is_empty() {
            println!(
                "\n  {} {}",
                "⚡".yellow(),
                format!("{} tests affected:", tests.len()).bold()
            );
            for t in &tests {
                println!(
                    "    {} {} ({})",
                    t.entity_type.dimmed(),
                    t.name.bold(),
                    t.file_path.dimmed(),
                );
            }
        }

        println!();
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

        let result = find_entity_in_graph(&graph, &all, Some("Process(int)"), None, None, "sem impact");
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

        let result = find_entity_in_graph(&graph, &all, Some("Process(string)"), None, None, "sem impact");
        assert_eq!(result.id, "svc.cs::method::Process(string)");
        assert_eq!(result.start_line, 30);
    }

    #[test]
    fn test_find_entity_single_no_signature_needed() {
        let entities = vec![
            make_entity("Handle", None, 50),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("Handle"), None, None, "sem impact");
        assert_eq!(result.name, "Handle");
    }

    #[test]
    fn test_find_entity_by_id_bypasses_name() {
        let entities = vec![
            make_entity("Process", Some("(int)"), 10),
            make_entity("Process", Some("(string)"), 30),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, None, Some("svc.cs::method::Process(string)"), None, "sem impact");
        assert_eq!(result.id, "svc.cs::method::Process(string)");
    }

    #[test]
    fn test_find_entity_file_hint_with_overloads() {
        let mut e1 = make_entity("Process", Some("(int)"), 10);
        e1.file_path = "a.cs".to_string();
        e1.id = "a.cs::method::Process(int)".to_string();
        let mut e2 = make_entity("Process", Some("(int)"), 20);
        e2.file_path = "b.cs".to_string();
        e2.id = "b.cs::method::Process(int)".to_string();
        let entities = vec![e1, e2];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("Process(int)"), None, Some("b.cs"), "sem impact");
        assert_eq!(result.file_path, "b.cs");
    }

    #[test]
    fn test_find_entity_single_overload_no_signature_ok() {
        let entities = vec![
            make_entity("Process", Some("(int)"), 10),
            make_entity("Handle", None, 50),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("Process"), None, None, "sem impact");
        assert_eq!(result.id, "svc.cs::method::Process(int)");
    }

    #[test]
    fn test_find_entity_empty_params_matches_no_signature() {
        let entities = vec![
            make_entity("ResumeAllDown", Some("()"), 199),
            make_entity("ResumeAllDown", Some("(bool)"), 467),
        ];
        let (graph, all) = build_graph(&entities);

        let result = find_entity_in_graph(&graph, &all, Some("ResumeAllDown()"), None, None, "sem impact");
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

        let result = find_entity_in_graph(&graph, &all, Some("ResumeAllDown(bool)"), None, None, "sem impact");
        assert_eq!(result.id, "svc.cs::method::ResumeAllDown(bool)");
        assert_eq!(result.start_line, 467);
    }
}
