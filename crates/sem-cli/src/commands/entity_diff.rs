use colored::Colorize;
use sem_core::git::bridge::GitBridge;
use sem_core::model::change::{ChangeType, SemanticChange};
use sem_core::model::entity::SemanticEntity;
use sem_core::model::identity::parent_name;
use sem_core::parser::differ::DiffResult;
use crate::formatters::terminal;
use std::collections::HashMap;
use std::path::Path;

pub struct EntityDiffOptions {
    pub cwd: String,
    pub entity: String,
    pub from_ref: String,
    pub to_ref: String,
    pub file: Option<String>,
    pub verbose: bool,
}

pub fn entity_diff_command(opts: EntityDiffOptions) {
    let root = Path::new(&opts.cwd);
    let registry = super::create_registry(&opts.cwd);
    let query = super::parse_entity_query(&opts.entity);

    // Resolve jj revsets
    let mut from_ref = opts.from_ref;
    let mut to_ref = opts.to_ref;
    if sem_core::git::jj::is_jj_repo(root) {
        from_ref = sem_core::git::jj::maybe_resolve_ref(&from_ref, root);
        to_ref = sem_core::git::jj::maybe_resolve_ref(&to_ref, root);
    }

    let bridge = match GitBridge::open(root) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{} {}", "error:".red().bold(), e);
            std::process::exit(1);
        }
    };

    // Auto-detect file if not provided
    let git_file = if let Some(ref f) = opts.file {
        f.replace('\\', "/")
    } else {
        match super::log::find_entity_file(root, &registry, &query) {
            super::log::FindResult::Found(fp) => fp,
            super::log::FindResult::Ambiguous(files) => {
                eprintln!(
                    "{} Entity '{}' found in multiple files. Use --file to disambiguate:",
                    "error:".red().bold(),
                    opts.entity
                );
                for f in &files {
                    eprintln!("  {}", f);
                }
                std::process::exit(1);
            }
            super::log::FindResult::NotFound => {
                eprintln!(
                    "{} Entity '{}' not found in any file",
                    "error:".red().bold(),
                    opts.entity
                );
                std::process::exit(1);
            }
        }
    };

    // Resolve entity at each ref (handles cross-file moves)
    let from_resolved = resolve_entity_at_ref(&bridge, &registry, &query, &from_ref, &git_file, &to_ref);
    let to_resolved = resolve_entity_at_ref(&bridge, &registry, &query, &to_ref, &git_file, &from_ref);

    let (from_ent, from_file) = match from_resolved {
        Some(r) => r,
        None => {
            eprintln!(
                "{} Entity '{}' not found at ref '{}'",
                "error:".red().bold(),
                opts.entity,
                from_ref
            );
            std::process::exit(1);
        }
    };

    let (to_ent, to_file) = match to_resolved {
        Some(r) => r,
        None => {
            eprintln!(
                "{} Entity '{}' not found at ref '{}'",
                "error:".red().bold(),
                opts.entity,
                to_ref
            );
            std::process::exit(1);
        }
    };

    // Build DiffResult with a single change
    let to_by_id: HashMap<&str, &SemanticEntity> = std::iter::once((to_ent.id.as_str(), &to_ent)).collect();

    let content_changed = from_ent.content != to_ent.content;
    let signature_changed = from_ent.signature != to_ent.signature;
    let name_changed = from_ent.name != to_ent.name;
    let file_changed = from_file != to_file;

    let (change_type, entity_name, old_entity_name, signature, old_signature) =
        if !content_changed && !signature_changed && !name_changed {
            // No change
            println!("{}", "No changes detected for this entity.".dimmed());
            return;
        } else if file_changed && name_changed {
            (ChangeType::Moved, to_ent.name.clone(), Some(from_ent.name.clone()),
             to_ent.signature.clone(), from_ent.signature.clone())
        } else if file_changed {
            (ChangeType::Moved, to_ent.name.clone(), None,
             to_ent.signature.clone(), from_ent.signature.clone())
        } else if name_changed && content_changed {
            (ChangeType::Renamed, to_ent.name.clone(), Some(from_ent.name.clone()),
             to_ent.signature.clone(), from_ent.signature.clone())
        } else if signature_changed && content_changed {
            (ChangeType::SignatureChanged, to_ent.name.clone(), None,
             to_ent.signature.clone(), from_ent.signature.clone())
        } else {
            (ChangeType::Modified, to_ent.name.clone(), None,
             to_ent.signature.clone(), from_ent.signature.clone())
        };

    let parent = parent_name(&to_ent, &to_by_id);
    let change = SemanticChange {
        id: format!("entity-diff-{}", to_ent.id),
        entity_id: to_ent.id.clone(),
        change_type,
        entity_type: to_ent.entity_type.clone(),
        entity_name,
        entity_line: to_ent.start_line,
        parent_name: parent,
        file_path: to_file.clone(),
        old_entity_name,
        signature,
        old_signature,
        old_file_path: if file_changed { Some(from_file.clone()) } else { None },
        old_parent_id: None,
        before_content: if content_changed {
            Some(from_ent.content.clone())
        } else {
            None
        },
        after_content: if content_changed {
            Some(to_ent.content.clone())
        } else {
            None
        },
        commit_sha: None,
        author: None,
        timestamp: None,
        structural_change: if content_changed { Some(true) } else { None },
    };

    let result = DiffResult {
        changes: vec![change],
        file_count: 1,
        added_count: 0,
        modified_count: 1,
        deleted_count: 0,
        moved_count: if file_changed { 1 } else { 0 },
        renamed_count: if name_changed && !file_changed { 1 } else { 0 },
        reordered_count: 0,
        signature_changed_count: if signature_changed && !name_changed { 1 } else { 0 },
        orphan_count: 0,
        total_entities_before: 1,
        total_entities_after: 1,
    };

    println!(
        "{}",
        terminal::format_terminal(&result, opts.verbose)
    );
}

/// Find a single entity matching the query, with disambiguation errors.
fn find_entity<'a>(
    entities: &'a [SemanticEntity],
    query: &super::EntityQuery,
) -> Result<&'a SemanticEntity, String> {
    let by_id: HashMap<&str, &SemanticEntity> = entities
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();

    let matching: Vec<&SemanticEntity> = entities
        .iter()
        .filter(|e| e.name == query.name)
        .filter(|e| {
            query
                .signature
                .as_ref()
                .map_or(true, |sig| e.signature.as_deref() == Some(sig.as_str()))
        })
        .filter(|e| {
            query
                .scope
                .as_ref()
                .map_or(true, |scope| super::scope_matches(e, scope, &by_id))
        })
        .collect();

    if matching.is_empty() {
        // Try name-only to give better error
        let name_only: Vec<&SemanticEntity> = entities.iter().filter(|e| e.name == query.name).collect();
        if !name_only.is_empty() && query.scope.is_some() {
            return Err(format!(
                "Entity '{}' not found in scope '{}'",
                query.name,
                query.scope.as_ref().unwrap().join(".")
            ));
        }
        if !name_only.is_empty() && query.signature.is_some() {
            return Err(format!(
                "Entity '{}' not found with signature {}",
                query.name,
                query.signature.as_ref().unwrap()
            ));
        }
        return Err(format!("Entity '{}' not found", query.name));
    }

    if matching.len() == 1 {
        return Ok(matching[0]);
    }

    // Multiple matches — disambiguate
    let unique_parents: Vec<Option<&str>> = matching
        .iter()
        .map(|e| e.parent_id.as_deref())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if unique_parents.len() > 1 {
        let scopes: Vec<String> = matching
            .iter()
            .map(|e| {
                parent_name(e, &by_id).unwrap_or_else(|| "(top-level)".to_string())
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        return Err(format!(
            "Entity '{}' exists in {} types: {}. Use 'Type.{}' to disambiguate",
            query.name,
            unique_parents.len(),
            scopes.join(", "),
            query.name
        ));
    }

    // True overloads
    let sigs: Vec<String> = matching
        .iter()
        .map(|e| e.signature.as_deref().unwrap_or("()").to_string())
        .collect();
    Err(format!(
        "Entity '{}' has {} overloads: {}. Specify the signature to disambiguate",
        query.name,
        matching.len(),
        sigs.join(", ")
    ))
}

/// Try to find entity with scope, then fall back to name+signature only.
/// This handles cases where parent_id is missing at an older ref (parser
/// didn't detect the parent class, e.g. C# partial classes).
fn find_with_scope_fallback<'a>(
    entities: &'a [SemanticEntity],
    query: &super::EntityQuery,
) -> Option<&'a SemanticEntity> {
    // Try with scope first
    if let Ok(ent) = find_entity(entities, query) {
        return Some(ent);
    }
    // Fallback: retry without scope (parent may not have been detected)
    if query.scope.is_some() {
        let relaxed = super::EntityQuery {
            name: query.name.clone(),
            signature: query.signature.clone(),
            scope: None,
        };
        if let Ok(ent) = find_entity(entities, &relaxed) {
            return Some(ent);
        }
    }
    None
}

/// Resolve an entity at a given ref. If the primary file doesn't exist at that ref
/// (entity moved), search candidate files using git diff and scope-based heuristics.
/// Returns (entity, file_path) or None if not found.
fn resolve_entity_at_ref(
    bridge: &GitBridge,
    registry: &sem_core::parser::registry::ParserRegistry,
    query: &super::EntityQuery,
    git_ref: &str,
    primary_file: &str,
    other_ref: &str,
) -> Option<(SemanticEntity, String)> {
    // Try the primary file first
    let primary_result = bridge.read_file_at_ref(git_ref, primary_file);
    if let Ok(Some(content)) = primary_result {
        let entities = registry.extract_entities(primary_file, &content);
        if let Some(r) = find_with_scope_fallback(&entities, query) {
            return Some((r.clone(), primary_file.to_string()));
        }
    }

    // Primary file not found or entity not in it — find candidate files
    let candidates = collect_candidate_files(bridge, query, primary_file, git_ref, other_ref);

    for file_path in &candidates {
        if let Ok(Some(content)) = bridge.read_file_at_ref(git_ref, file_path) {
            let entities = registry.extract_entities(file_path, &content);
            if let Some(r) = find_with_scope_fallback(&entities, query) {
                return Some((r.clone(), file_path.clone()));
            }
        }
    }

    None
}

/// Collect candidate files where the entity might exist at a given ref.
/// Uses git diff to find renamed/moved files and scope-based path heuristics.
fn collect_candidate_files(
    bridge: &GitBridge,
    query: &super::EntityQuery,
    primary_file: &str,
    git_ref: &str,
    other_ref: &str,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(primary_file.to_string());

    // Strategy 1: Use git diff between the two refs to find renamed/moved files.
    // The entity might have moved from one file to another.
    if let Ok(changes) = bridge.get_changed_files(
        &sem_core::git::types::DiffScope::Range {
            from: other_ref.to_string(),
            to: git_ref.to_string(),
        },
        &[],
    ) {
        for change in &changes {
            // Check the current file path at git_ref
            if !seen.contains(&change.file_path) {
                candidates.push(change.file_path.clone());
                seen.insert(change.file_path.clone());
            }
            // Check the old file path (before rename/move)
            if let Some(ref old_path) = change.old_file_path {
                if !seen.contains(old_path.as_str()) {
                    candidates.push(old_path.clone());
                    seen.insert(old_path.clone());
                }
            }
        }
    }

    // Strategy 2: Scope-based heuristic. If query has scope like "AccountService",
    // look for files with that name in the same directory as the primary file.
    if let Some(ref scope) = query.scope {
        for scope_part in scope {
            let dir = primary_file.rfind('/').map(|i| &primary_file[..i]).unwrap_or("");
            let ext = primary_file.rfind('.').map(|i| &primary_file[i..]).unwrap_or(".cs");
            for candidate in &[
                format!("{}/{}{}", dir, scope_part, ext),
                format!("{}/{}.cs", dir, scope_part), // fallback to .cs
            ] {
                if !candidate.is_empty() && !seen.contains(candidate) {
                    candidates.push(candidate.clone());
                    seen.insert(candidate.clone());
                }
            }
        }
    }

    candidates
}
