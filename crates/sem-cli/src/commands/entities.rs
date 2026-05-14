use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use colored::Colorize;
use sem_core::model::entity::SemanticEntity;
use sem_core::parser::registry::ParserRegistry;

pub struct EntitiesOptions {
    pub cwd: String,
    pub path: Option<String>,
    pub json: bool,
}

pub fn entities_command(opts: EntitiesOptions) {
    let root = Path::new(&opts.cwd);
    let registry = super::create_registry(&opts.cwd);
    let path_arg = opts.path.as_deref().filter(|p| !p.is_empty()).unwrap_or(".");
    let (path_label, full_path) = resolve_path(root, path_arg);

    let (entities, include_file) = if full_path.is_file() {
        (
            extract_file_entities(&full_path, &registry, &path_label).unwrap_or_else(|e| {
                eprintln!(
                    "{} Cannot read '{}': {}",
                    "error:".red().bold(),
                    path_label,
                    e
                );
                std::process::exit(1);
            }),
            false,
        )
    } else if full_path.is_dir() {
        let file_paths = find_supported_files_in_path(root, &full_path, &registry);
        (extract_files_entities(root, &file_paths, &registry), true)
    } else {
        eprintln!("{} Path not found '{}'", "error:".red().bold(), path_arg);
        std::process::exit(1);
    };

    if opts.json {
        let overload_keys = collect_overload_keys(&entities);
        let output: Vec<_> = entities
            .iter()
            .map(|e| {
                let is_overload = overload_keys.contains(&entity_logical_key(e));
                entity_json(e, include_file, is_overload)
            })
            .collect();
        println!("{}", serde_json::to_string(&output).unwrap());
    } else if should_group_by_file(&entities) {
        let overload_keys = collect_overload_keys(&entities);
        print_grouped_entities(&path_label, &entities, &overload_keys);
    } else if let Some(file_path) = entities.first().map(|e| e.file_path.as_str()) {
        let overload_keys = collect_overload_keys(&entities);
        print_file_entities(file_path, &entities, &overload_keys);
    } else {
        println!("{} {}\n", "entities:".green().bold(), path_label.bold());
    }
}

fn resolve_path(root: &Path, path_arg: &str) -> (String, PathBuf) {
    let path = Path::new(path_arg);
    let full_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };

    let label = if path.is_absolute() {
        file_path_for_entity(root, &full_path)
    } else {
        path_arg.to_string()
    };

    (label, full_path)
}

fn find_supported_files_in_path(
    root: &Path,
    scan_path: &Path,
    registry: &ParserRegistry,
) -> Vec<String> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(scan_path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!(
                    "{} Cannot walk '{}': {}",
                    "error:".red().bold(),
                    scan_path.display(),
                    e
                );
                std::process::exit(1);
            }
        };

        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_path = file_path_for_entity(root, path);
        if registry.get_plugin(&file_path).is_some() {
            files.push(file_path);
        }
    }

    files.sort();
    files
}

fn extract_files_entities(
    root: &Path,
    file_paths: &[String],
    registry: &ParserRegistry,
) -> Vec<SemanticEntity> {
    let mut entities = Vec::new();
    for file_path in file_paths {
        match extract_file_entities(&root.join(file_path), registry, file_path) {
            Ok(new_ents) => entities.extend(new_ents),
            Err(e) => {
                eprintln!(
                    "{} Cannot read '{}': {}",
                    "error:".red().bold(),
                    file_path,
                    e
                );
            }
        }
    }
    entities
}

fn file_path_for_entity(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn extract_file_entities(
    full_path: &Path,
    registry: &ParserRegistry,
    file_path: &str,
) -> Result<Vec<SemanticEntity>, std::io::Error> {
    let content = std::fs::read_to_string(&full_path)?;
    Ok(registry.extract_entities(file_path, &content))
}

/// Compute the set of logical keys that have overloads (multiple entities with same file+parent+name).
fn collect_overload_keys(entities: &[SemanticEntity]) -> BTreeSet<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in entities {
        let key = match &e.parent_id {
            Some(p) => format!("{}::{}::{}", e.file_path, p, e.name),
            None => format!("{}::{}::{}", e.file_path, e.entity_type, e.name),
        };
        *counts.entry(key).or_default() += 1;
    }
    counts.into_iter().filter(|(_, c)| *c > 1).map(|(k, _)| k).collect()
}

fn entity_logical_key(entity: &SemanticEntity) -> String {
    match &entity.parent_id {
        Some(p) => format!("{}::{}::{}", entity.file_path, p, entity.name),
        None => format!("{}::{}::{}", entity.file_path, entity.entity_type, entity.name),
    }
}

fn entity_json(entity: &SemanticEntity, include_file: bool, is_overload: bool) -> serde_json::Value {
    let mut value = serde_json::json!({
        "name": entity.name,
        "type": entity.entity_type,
        "start_line": entity.start_line,
        "end_line": entity.end_line,
        "parent_id": entity.parent_id,
    });

    if include_file {
        value["file"] = serde_json::json!(entity.file_path);
    }

    if is_overload {
        if let Some(sig) = &entity.signature {
            value["signature"] = serde_json::Value::String(sig.clone());
        }
    }

    value
}

fn print_file_entities(file_path: &str, entities: &[SemanticEntity], overload_keys: &BTreeSet<String>) {
    println!("{} {}\n", "entities:".green().bold(), file_path.bold());
    print_entity_rows(entities, "  ", overload_keys);
}

fn should_group_by_file(entities: &[SemanticEntity]) -> bool {
    let files: BTreeSet<&str> = entities.iter().map(|e| e.file_path.as_str()).collect();
    files.len() > 1
}

fn print_grouped_entities(path_label: &str, entities: &[SemanticEntity], overload_keys: &BTreeSet<String>) {
    println!("{} {}\n", "entities:".green().bold(), path_label.bold());

    let mut current_file: Option<&str> = None;
    for entity in entities {
        if current_file != Some(entity.file_path.as_str()) {
            current_file = Some(entity.file_path.as_str());
            println!("  {}", entity.file_path.bold());
        }

        let indent = if entity.parent_id.is_some() {
            "      "
        } else {
            "    "
        };
        let is_overload = overload_keys.contains(&entity_logical_key(entity));
        print_entity_row(entity, indent, is_overload);
    }
}

fn print_entity_rows(entities: &[SemanticEntity], base_indent: &str, overload_keys: &BTreeSet<String>) {
    for entity in entities {
        let indent = if entity.parent_id.is_some() {
            format!("{base_indent}  ")
        } else {
            base_indent.to_string()
        };
        let is_overload = overload_keys.contains(&entity_logical_key(entity));
        print_entity_row(entity, &indent, is_overload);
    }
}

fn print_entity_row(entity: &SemanticEntity, indent: &str, is_overload: bool) {
    let sig_display = if is_overload {
        match &entity.signature {
            Some(sig) => format!("{}", sig.dimmed()),
            None => String::new(),
        }
    } else {
        String::new()
    };
    println!(
        "{}{} {}{} (L{}:{})",
        indent,
        entity.entity_type.dimmed(),
        entity.name.bold(),
        sig_display,
        entity.start_line,
        entity.end_line,
    );
}
