use colored::Colorize;
use sem_core::git::bridge::GitBridge;
use sem_core::model::change::{ChangeType, SemanticChange};
use sem_core::model::entity::SemanticEntity;
use sem_core::model::identity::parent_name;
use sem_core::parser::differ::DiffResult;
use crate::formatters::terminal;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

pub struct EntityDiffOptions {
    pub cwd: String,
    pub entity: String,
    pub from_ref: String,
    pub to_ref: String,
    pub file: Option<String>,
    pub verbose: bool,
    pub profile: bool,
}

pub fn entity_diff_command(opts: EntityDiffOptions) {
    let total_start = Instant::now();
    let root = Path::new(&opts.cwd);
    let t0 = Instant::now();
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
    let registry_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Auto-detect file if not provided
    let t1 = Instant::now();
    let git_file = if let Some(ref f) = opts.file {
        f.replace('\\', "/")
    } else {
        match super::log::find_entity_file(root, &registry, &query, Some(&bridge)) {
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
    let find_file_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // If query has no scope, discover parent name from the file on disk
    // so resolve_entity_at_ref can use scope heuristic instead of git diff
    let effective_scope = if query.scope.is_none() {
        let file_on_disk = root.join(&git_file);
        if let Ok(content) = std::fs::read_to_string(&file_on_disk) {
            let entities = registry.extract_entities(&git_file, &content);
            let by_id: HashMap<&str, &SemanticEntity> = entities.iter()
                .map(|e| (e.id.as_str(), e)).collect();
            entities.iter()
                .filter(|e| e.name == query.name)
                .filter(|e| {
                    query.signature.as_ref().map_or(true, |sig| {
                        e.signature.as_deref() == Some(sig.as_str())
                    })
                })
                .find_map(|e| parent_name(e, &by_id))
                .map(|pn| pn.split('.').map(|s| s.to_string()).collect())
        } else {
            None
        }
    } else {
        query.scope.clone()
    };

    // Resolve entity at each ref (handles cross-file moves)
    let t2 = Instant::now();
    let from_resolved = resolve_entity_at_ref(&bridge, &registry, &query, &effective_scope, &from_ref, &git_file, &to_ref);
    let to_resolved = resolve_entity_at_ref(&bridge, &registry, &query, &effective_scope, &to_ref, &git_file, &from_ref);
    let resolve_ms = t2.elapsed().as_secs_f64() * 1000.0;

    let (from_ent, from_file) = match from_resolved {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "{} Entity '{}' not found at ref '{}'",
                "error:".red().bold(),
                opts.entity,
                from_ref
            );
            std::process::exit(1);
        }
        Err(msg) => {
            eprintln!("{} {}", "error:".red().bold(), msg);
            std::process::exit(1);
        }
    };

    let (to_ent, to_file) = match to_resolved {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "{} Entity '{}' not found at ref '{}'",
                "error:".red().bold(),
                opts.entity,
                to_ref
            );
            std::process::exit(1);
        }
        Err(msg) => {
            eprintln!("{} {}", "error:".red().bold(), msg);
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

    let t3 = Instant::now();
    println!(
        "{}",
        terminal::format_terminal(&result, opts.verbose)
    );
    let format_ms = t3.elapsed().as_secs_f64() * 1000.0;

    if opts.profile {
        let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;
        eprintln!();
        eprintln!("\x1b[2m── Profile ──────────────────────────────────\x1b[0m");
        eprintln!("\x1b[2m  registry init        {registry_ms:>8.2}ms\x1b[0m");
        eprintln!("\x1b[2m  find_entity_file     {find_file_ms:>8.2}ms\x1b[0m");
        eprintln!("\x1b[2m  resolve refs         {resolve_ms:>8.2}ms\x1b[0m");
        eprintln!("\x1b[2m  format output        {format_ms:>8.2}ms\x1b[0m");
        eprintln!("\x1b[2m  ─────────────────────────────────────────────\x1b[0m");
        eprintln!("\x1b[2m  total                {total_ms:>8.2}ms\x1b[0m");
        eprintln!("\x1b[2m  file: {}\x1b[0m", git_file);
        eprintln!("\x1b[2m─────────────────────────────────────────────\x1b[0m");
    }
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

    // True overloads — show each overload with a copy-pasteable command
    let scope_prefix = query.scope.as_ref().map_or(String::new(), |s| s.join(".") + ".");
    let overload_list: Vec<String> = matching
        .iter()
        .map(|e| {
            let sig = e.signature.as_deref().unwrap_or("()");
            let scope_label = parent_name(e, &by_id).unwrap_or_else(|| "(top-level)".to_string());
            format!(
                "  {}.{}{} (L{}:{})",
                scope_label, e.name, sig, e.start_line, e.end_line
            )
        })
        .collect();
    Err(format!(
        "Entity '{}' has {} overloads:\n{}\n\nSpecify the signature, e.g.:\n  sem entity-diff \"{}{}{}\" <from> <to>",
        query.name,
        matching.len(),
        overload_list.join("\n"),
        scope_prefix,
        query.name,
        matching[0].signature.as_deref().unwrap_or("()")
    ))
}

/// Resolve an entity at a given ref. If the primary file doesn't exist at that ref
/// (entity moved), search candidate files using scope-based heuristics first (cheap),
/// then git diff (expensive) as fallback.
/// Returns Ok(entity, file_path), Err(disambiguation message), or None if not found.
fn resolve_entity_at_ref(
    bridge: &GitBridge,
    registry: &sem_core::parser::registry::ParserRegistry,
    query: &super::EntityQuery,
    effective_scope: &Option<Vec<String>>,
    git_ref: &str,
    primary_file: &str,
    other_ref: &str,
) -> Result<Option<(SemanticEntity, String)>, String> {
    // Try the primary file first — only exact matches; any miss falls through to candidates
    if let Ok(Some(content)) = bridge.read_file_at_ref(git_ref, primary_file) {
        let entities = registry.extract_entities(primary_file, &content);
        if let Ok(r) = find_entity(&entities, query) {
            return Ok(Some((r.clone(), primary_file.to_string())));
        }
    }

    // Strategy 1 (cheap): scope-based heuristic candidates
    let scope_candidates = scope_candidate_files(effective_scope, primary_file);
    if let Some(result) = try_candidates(bridge, registry, query, git_ref, &scope_candidates) {
        return result;
    }

    // Strategy 2 (expensive): git diff between the two refs
    let git_candidates = git_diff_candidate_files(bridge, primary_file, git_ref, other_ref);
    if let Some(result) = try_candidates(bridge, registry, query, git_ref, &git_candidates) {
        return result;
    }

    Ok(None)
}

/// Try a list of candidate files, returning the first exact match.
/// Tracks the best error if entity name exists but no exact match.
fn try_candidates(
    bridge: &GitBridge,
    registry: &sem_core::parser::registry::ParserRegistry,
    query: &super::EntityQuery,
    git_ref: &str,
    candidates: &[String],
) -> Option<Result<Option<(SemanticEntity, String)>, String>> {
    let mut name_found_error: Option<String> = None;

    for file_path in candidates {
        if let Ok(Some(content)) = bridge.read_file_at_ref(git_ref, file_path) {
            let entities = registry.extract_entities(file_path, &content);
            match find_entity(&entities, query) {
                Ok(r) => return Some(Ok(Some((r.clone(), file_path.clone())))),
                Err(msg) => {
                    if entities.iter().any(|e| e.name == query.name) {
                        if name_found_error.is_none() {
                            name_found_error = Some(msg);
                        }
                    }
                }
            }
        }
    }

    // If we found the name somewhere but never exact-matched, return that error
    name_found_error.map(Err)
}

/// Generate scope-based candidate file paths (cheap, no git operations).
/// Uses effective_scope which may be discovered from disk if query had no scope.
fn scope_candidate_files(effective_scope: &Option<Vec<String>>, primary_file: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(primary_file.to_string());

    if let Some(ref scope) = effective_scope {
        for scope_part in scope {
            // Handle both / and \ as path separators (Windows compatibility)
            let dir = primary_file.rfind(|c| c == '/' || c == '\\')
                .map(|i| &primary_file[..i])
                .unwrap_or("");
            let ext = primary_file.rfind('.')
                .map(|i| &primary_file[i..])
                .unwrap_or(".cs");
            let sep = if primary_file.contains('\\') { "\\" } else { "/" };
            for candidate in &[
                format!("{}{}{}{}", dir, if dir.is_empty() { "" } else { sep }, scope_part, ext),
                format!("{}{}{}.cs", dir, if dir.is_empty() { "" } else { sep }, scope_part),
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

/// Generate candidate files from git diff between refs (expensive).
fn git_diff_candidate_files(
    bridge: &GitBridge,
    primary_file: &str,
    git_ref: &str,
    other_ref: &str,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(primary_file.to_string());

    if let Ok(changes) = bridge.get_changed_files(
        &sem_core::git::types::DiffScope::Range {
            from: other_ref.to_string(),
            to: git_ref.to_string(),
        },
        &[],
    ) {
        for change in &changes {
            if !seen.contains(&change.file_path) {
                candidates.push(change.file_path.clone());
                seen.insert(change.file_path.clone());
            }
            if let Some(ref old_path) = change.old_file_path {
                if !seen.contains(old_path.as_str()) {
                    candidates.push(old_path.clone());
                    seen.insert(old_path.clone());
                }
            }
        }
    }

    candidates
}
