use std::collections::HashMap;
use std::path::Path;

use colored::Colorize;
use sem_core::git::bridge::GitBridge;
use sem_core::model::entity::SemanticEntity;
use sem_core::parser::registry::ParserRegistry;

use super::truncate_str;

/// Parsed entity query: "Name" or "Name(Signature)".
struct EntityQuery {
    name: String,
    signature: Option<String>,
}

fn parse_entity_query(input: &str) -> EntityQuery {
    if let Some(open) = input.rfind('(') {
        if input.ends_with(')') && open > 0 {
            return EntityQuery {
                name: input[..open].to_string(),
                signature: Some(input[open..].to_string()),
            };
        }
    }
    EntityQuery {
        name: input.to_string(),
        signature: None,
    }
}

pub struct LogOptions {
    pub cwd: String,
    pub entity_name: String,
    pub file_path: Option<String>,
    pub limit: usize,
    pub json: bool,
    pub verbose: bool,
}

#[derive(Debug)]
enum EntityChangeType {
    Added,
    ModifiedLogic,
    ModifiedCosmetic,
    Deleted,
    Moved,
    Reappeared,
    SignatureChanged,
    Renamed,
}

impl EntityChangeType {
    fn label(&self) -> &str {
        match self {
            EntityChangeType::Added => "added",
            EntityChangeType::ModifiedLogic => "modified (logic)",
            EntityChangeType::ModifiedCosmetic => "modified (cosmetic)",
            EntityChangeType::Deleted => "deleted",
            EntityChangeType::Moved => "moved",
            EntityChangeType::Reappeared => "reappeared",
            EntityChangeType::SignatureChanged => "signature",
            EntityChangeType::Renamed => "renamed",
        }
    }

    fn label_colored(&self) -> colored::ColoredString {
        match self {
            EntityChangeType::Added => "added".green(),
            EntityChangeType::ModifiedLogic => "modified (logic)".yellow(),
            EntityChangeType::ModifiedCosmetic => "modified (cosmetic)".dimmed(),
            EntityChangeType::Deleted => "deleted".red(),
            EntityChangeType::Moved => "moved".blue(),
            EntityChangeType::Reappeared => "reappeared".green(),
            EntityChangeType::SignatureChanged => "signature".magenta(),
            EntityChangeType::Renamed => "renamed".cyan(),
        }
    }
}

struct LogEntry {
    short_sha: String,
    author: String,
    date: String,
    message: String,
    change_type: EntityChangeType,
    content: Option<String>,
    prev_content: Option<String>,
    file_path: Option<String>,
    prev_file_path: Option<String>,
    old_name: Option<String>,
    old_signature: Option<String>,
}

pub fn log_command(opts: LogOptions) {
    let root = Path::new(&opts.cwd);
    let registry = super::create_registry(&opts.cwd);
    let mut query = parse_entity_query(&opts.entity_name);

    let bridge = match GitBridge::open(root) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{} {}", "error:".red().bold(), e);
            std::process::exit(1);
        }
    };

    // Resolve file path: use provided or auto-detect
    let file_path = match opts.file_path {
        Some(fp) => fp,
        None => match find_entity_file(root, &registry, &query) {
            FindResult::Found(fp) => fp,
            FindResult::Ambiguous(files) => {
                eprintln!(
                    "{} Entity '{}' found in multiple files:",
                    "error:".red().bold(),
                    opts.entity_name
                );
                for f in &files {
                    eprintln!("  {}", f);
                }
                eprintln!("\nUse --file to disambiguate.");
                std::process::exit(1);
            }
            FindResult::NotFound => {
                eprintln!(
                    "{} Entity '{}' not found in any file",
                    "error:".red().bold(),
                    opts.entity_name
                );
                std::process::exit(1);
            }
        },
    };

    // Convert file_path to be relative to git repo root (for git operations)
    let repo_root = bridge.repo_root();
    let abs_cwd = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let abs_repo = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let git_file_path = if abs_cwd != abs_repo {
        // cwd is a subdirectory of repo root, prepend the prefix
        let prefix = abs_cwd.strip_prefix(&abs_repo).unwrap_or(Path::new(""));
        prefix.join(&file_path).to_string_lossy().to_string()
    } else {
        file_path.clone()
    };

    // Verify the file has a parser (read content for shebang detection on extensionless files)
    let file_content_hint = std::fs::read_to_string(root.join(&file_path)).unwrap_or_default();
    let resolved_fp = registry.resolve_file_path(&file_path);
    let detection_fp = resolved_fp.as_deref().unwrap_or(&file_path);
    if registry.get_plugin_with_content(detection_fp, &file_content_hint).is_none() {
        eprintln!(
            "{} Unsupported file type: {}",
            "error:".red().bold(),
            file_path
        );
        std::process::exit(1);
    }

    // Walk commits, tracking entity across file moves
    // The outer 'walk loop supports restarting when a rename predecessor is detected.
    let mut current_git_file = git_file_path.clone();
    let mut entries: Vec<LogEntry> = Vec::new();
    let mut prev_entity_content: Option<String> = None;
    let mut prev_structural_hash: Option<String> = None;
    let mut prev_content_hash: Option<String> = None;
    let mut prev_entity_name: String = query.name.clone();
    let mut prev_entity_signature: Option<String> = query.signature.clone();
    let mut entity_type = String::new();
    let mut found_at_least_once = false;
    let mut total_commits = 0usize;
    let mut skip_until_sha: Option<String> = None;
    let mut restart_with: Option<(String, Option<String>)> = None;

    'walk: loop {
        if let Some((new_name, new_sig)) = restart_with.take() {
            query.name = new_name;
            query.signature = new_sig;
            current_git_file = git_file_path.clone();
            entries.clear();
            prev_entity_content = None;
            prev_structural_hash = None;
            prev_content_hash = None;
            prev_entity_name = query.name.clone();
            prev_entity_signature = query.signature.clone();
            entity_type = String::new();
            found_at_least_once = false;
            total_commits = 0;
            skip_until_sha = None;
        }

    loop {
        let commits = match bridge.get_file_commits(&current_git_file, opts.limit) {
            Ok(c) => c,
            Err(e) => {
                if total_commits == 0 {
                    eprintln!("{} Failed to get file history: {}", "error:".red().bold(), e);
                    std::process::exit(1);
                }
                break;
            }
        };

        if commits.is_empty() && total_commits == 0 {
            eprintln!("{} No commits found for {}", "warning:".yellow().bold(), current_git_file);
            return;
        }

        let reversed: Vec<_> = commits.iter().rev().collect();

        // After a file move, skip commits until the move commit
        let start_idx = if let Some(ref sha) = skip_until_sha {
            reversed
                .iter()
                .position(|c| c.sha == *sha)
                .map(|i| i + 1)
                .unwrap_or(reversed.len())
        } else {
            0
        };
        skip_until_sha = None;

        total_commits += reversed.len().saturating_sub(start_idx);
        let mut moved = false;

        for (idx, commit) in reversed[start_idx..].iter().enumerate() {
            let file_content = bridge
                .read_file_at_ref(&commit.sha, &current_git_file)
                .ok()
                .flatten();

            let commit_entities: Vec<SemanticEntity> = file_content
                .as_ref()
                .map(|c| registry.extract_entities(&current_git_file, c))
                .unwrap_or_default();

            let found_match = find_entity_in_commit(
                &commit_entities,
                &prev_entity_name,
                prev_entity_signature.as_deref(),
                &prev_entity_name,
                prev_entity_signature.as_deref(),
                prev_content_hash.as_deref(),
                prev_structural_hash.as_deref(),
                prev_entity_content.as_deref(),
            );

            let date = chrono_lite_format(commit.date.parse::<i64>().unwrap_or(0));
            let msg_first_line = commit.message.lines().next().unwrap_or("").to_string();

            match found_match {
                Some(m) => {
                    let ent = m.entity;
                    if !found_at_least_once {
                        entity_type = ent.entity_type.clone();
                    }

                    let cur_content_hash = ent.content_hash.clone();
                    let cur_structural_hash = ent.structural_hash.clone();

                    if !found_at_least_once {
                        found_at_least_once = true;
                        entries.push(LogEntry {
                            short_sha: commit.short_sha.clone(),
                            author: commit.author.clone(),
                            date,
                            message: msg_first_line,
                            change_type: EntityChangeType::Added,
                            content: Some(ent.content.clone()),
                            prev_content: None,
                            file_path: Some(current_git_file.clone()),
                            prev_file_path: None,
                            old_name: None,
                            old_signature: None,
                        });

                        // Lookback: check previous (older) commit for a renamed predecessor.
                        // If the entity was renamed, the commit just before this one would have
                        // an entity with a different name but very similar content.
                        let actual_idx = idx + start_idx;
                        if actual_idx > 0 {
                            let prev_commit = reversed[actual_idx - 1];
                            let prev_file_content = bridge
                                .read_file_at_ref(&prev_commit.sha, &current_git_file)
                                .ok()
                                .flatten();
                            let prev_entities: Vec<SemanticEntity> = prev_file_content
                                .as_ref()
                                .map(|c| registry.extract_entities(&current_git_file, c))
                                .unwrap_or_default();

                            let best_predecessor = prev_entities
                                .iter()
                                .filter(|e| e.name != query.name && e.entity_type == ent.entity_type)
                                .filter_map(|e| {
                                    let score = jaccard_similarity(&ent.content, &e.content);
                                    if score >= 0.5 { Some((e, score)) } else { None }
                                })
                                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

                            if let Some((pred, _score)) = best_predecessor {
                                restart_with = Some((pred.name.clone(), pred.signature.clone()));
                                continue 'walk;
                            }
                        }
                    } else if prev_entity_content.is_none() {
                        entries.push(LogEntry {
                            short_sha: commit.short_sha.clone(),
                            author: commit.author.clone(),
                            date,
                            message: msg_first_line,
                            change_type: EntityChangeType::Reappeared,
                            content: Some(ent.content.clone()),
                            prev_content: None,
                            file_path: Some(current_git_file.clone()),
                            prev_file_path: None,
                            old_name: None,
                            old_signature: None,
                        });
                    } else {
                        // Determine change type
                        let change_type = if let Some(mt) = m.match_type {
                            mt
                        } else {
                            // Identity-preserved match: check content change
                            let content_changed =
                                prev_content_hash.as_deref() != Some(cur_content_hash.as_str());

                            if content_changed {
                                let structural_changed =
                                    match (cur_structural_hash.as_deref(), prev_structural_hash.as_deref()) {
                                        (Some(cur), Some(prev)) => cur != prev,
                                        _ => true,
                                    };
                                if structural_changed {
                                    EntityChangeType::ModifiedLogic
                                } else {
                                    EntityChangeType::ModifiedCosmetic
                                }
                            } else {
                                // No change — skip this commit
                                prev_entity_content = Some(ent.content.clone());
                                prev_structural_hash = cur_structural_hash.clone();
                                prev_content_hash = Some(cur_content_hash.clone());
                                prev_entity_name = ent.name.clone();
                                prev_entity_signature = ent.signature.clone();
                                continue;
                            }
                        };

                        entries.push(LogEntry {
                            short_sha: commit.short_sha.clone(),
                            author: commit.author.clone(),
                            date,
                            message: msg_first_line,
                            change_type,
                            content: Some(ent.content.clone()),
                            prev_content: prev_entity_content.clone(),
                            file_path: Some(current_git_file.clone()),
                            prev_file_path: None,
                            old_name: m.old_name,
                            old_signature: m.old_signature,
                        });
                    }

                    prev_entity_content = Some(ent.content.clone());
                    prev_structural_hash = ent.structural_hash.clone();
                    prev_content_hash = Some(ent.content_hash.clone());
                    prev_entity_name = ent.name.clone();
                    prev_entity_signature = ent.signature.clone();
                }
                None => {
                    // Entity not in tracked file.
                    if prev_entity_content.is_some() {
                        // Already tracking — try cross-file search
                        let cross = search_entity_cross_file_v2(
                            &bridge,
                            &registry,
                            &commit.sha,
                            &prev_entity_name,
                            prev_entity_signature.as_deref(),
                            prev_content_hash.as_deref(),
                            prev_structural_hash.as_deref(),
                            &prev_entity_name,
                            prev_entity_signature.as_deref(),
                            &current_git_file,
                        );

                        match cross {
                            Some((new_file, ent, change_type, old_name, old_signature)) => {
                                let prev_file = current_git_file.clone();
                                entries.push(LogEntry {
                                    short_sha: commit.short_sha.clone(),
                                    author: commit.author.clone(),
                                    date,
                                    message: msg_first_line,
                                    change_type,
                                    content: Some(ent.content.clone()),
                                    prev_content: prev_entity_content.clone(),
                                    file_path: Some(new_file.clone()),
                                    prev_file_path: Some(prev_file),
                                    old_name,
                                    old_signature,
                                });

                                prev_entity_content = Some(ent.content.clone());
                                prev_structural_hash = ent.structural_hash.clone();
                                prev_content_hash = Some(ent.content_hash.clone());
                                prev_entity_name = ent.name.clone();
                                prev_entity_signature = ent.signature.clone();
                                skip_until_sha = Some(commit.sha.clone());
                                current_git_file = new_file;
                                moved = true;
                                break;
                            }
                            None => {
                                entries.push(LogEntry {
                                    short_sha: commit.short_sha.clone(),
                                    author: commit.author.clone(),
                                    date,
                                    message: msg_first_line,
                                    change_type: EntityChangeType::Deleted,
                                    content: None,
                                    prev_content: prev_entity_content.take(),
                                    file_path: Some(current_git_file.clone()),
                                    prev_file_path: None,
                                    old_name: None,
                                    old_signature: None,
                                });
                                prev_structural_hash = None;
                                prev_content_hash = None;
                            }
                        }
                    } else if query.signature.is_some() {
                        // Not yet tracking. Try to find a predecessor: same name,
                        // different signature, matching parameter count.
                        // This handles the case where the user queries by the NEW signature
                        // but the entity previously had a different signature.
                        if let Some(pred) = find_predecessor(&commit_entities, &query.name, query.signature.as_deref()) {
                            found_at_least_once = true;
                            entity_type = pred.entity_type.clone();
                            entries.push(LogEntry {
                                short_sha: commit.short_sha.clone(),
                                author: commit.author.clone(),
                                date,
                                message: msg_first_line,
                                change_type: EntityChangeType::Added,
                                content: Some(pred.content.clone()),
                                prev_content: None,
                                file_path: Some(current_git_file.clone()),
                                prev_file_path: None,
                                old_name: None,
                                old_signature: None,
                            });
                            prev_entity_content = Some(pred.content.clone());
                            prev_structural_hash = pred.structural_hash.clone();
                            prev_content_hash = Some(pred.content_hash.clone());
                            prev_entity_name = pred.name.clone();
                            prev_entity_signature = pred.signature.clone();
                        }
                    }
                }
            }
        }

        if !moved {
            break;
        }
    }

    if !found_at_least_once {
        eprintln!(
            "{} Entity '{}' not found in any commit of {}",
            "error:".red().bold(),
            opts.entity_name,
            file_path
        );
        std::process::exit(1);
    }

    let first_seen = entries.first().map(|e| e.date.clone()).unwrap_or_default();
    // Use the last file the entity was seen in for the header
    let display_file = entries
        .iter()
        .rev()
        .find_map(|e| e.file_path.as_ref())
        .unwrap_or(&file_path)
        .clone();
    // Check if entity ever moved between files
    let was_file = entries
        .iter()
        .find_map(|e| {
            if matches!(e.change_type, EntityChangeType::Moved) {
                e.prev_file_path.as_ref().cloned()
            } else {
                None
            }
        });

    if opts.json {
        print_json(&query, &display_file, &entity_type, &entries, opts.verbose);
    } else {
        print_terminal(&query, &display_file, was_file.as_deref(), &entity_type, &entries, total_commits, &first_seen, opts.verbose);
    }
    break 'walk;
    }
}

fn print_terminal(
    query: &EntityQuery,
    file_path: &str,
    was_file: Option<&str>,
    entity_type: &str,
    entries: &[LogEntry],
    total_commits: usize,
    first_seen: &str,
    verbose: bool,
) {
    let entity_display = match &query.signature {
        Some(sig) => format!("{}{}", query.name, sig),
        None => query.name.clone(),
    };
    let header = if let Some(prev) = was_file {
        format!(
            "┌─ {} :: {} :: {}  (was: {})",
            file_path, entity_type, entity_display, prev
        )
    } else {
        format!("┌─ {} :: {} :: {}", file_path, entity_type, entity_display)
    };
    println!("{}", header.bold());
    println!("│");

    let max_author_len = entries.iter().map(|e| e.author.len()).max().unwrap_or(6);
    let max_change_len = entries
        .iter()
        .map(|e| {
            let label = compute_display_label(e);
            label.len()
        })
        .max()
        .unwrap_or(10);

    for entry in entries {
        let msg_short = truncate_str(&entry.message, 50);
        let display_label = compute_display_label(entry);
        let display_colored = compute_display_label_colored(entry);

        println!(
            "│  {}  {:<max_author$}  {}  {:<max_change$}  {}",
            entry.short_sha.yellow(),
            entry.author.cyan(),
            entry.date.dimmed(),
            display_colored,
            msg_short,
            max_author = max_author_len,
            max_change = max_change_len,
        );

        // Show file transition for Moved entries
        if matches!(entry.change_type, EntityChangeType::Moved) {
            if let Some(new_fp) = &entry.file_path {
                println!(
                    "│    {}",
                    format!("→ moved to {}", new_fp).blue()
                );
            }
        }

        // Show signature change details
        if matches!(entry.change_type, EntityChangeType::SignatureChanged) {
            if let Some(old_sig) = &entry.old_signature {
                println!(
                    "│    {}",
                    format!("→ signature {}", old_sig).magenta()
                );
            }
        }

        // Show rename details
        if matches!(entry.change_type, EntityChangeType::Renamed) {
            if let Some(old_name) = &entry.old_name {
                println!(
                    "│    {}",
                    format!("→ renamed {} → ...", old_name).cyan()
                );
            }
        }

        // Show combined moved + signature/rename details
        if matches!(entry.change_type, EntityChangeType::Moved) {
            if let Some(old_sig) = &entry.old_signature {
                println!(
                    "│    {}",
                    format!("→ signature {}", old_sig).magenta()
                );
            }
            if let Some(old_name) = &entry.old_name {
                println!(
                    "│    {}",
                    format!("→ renamed {}", old_name).cyan()
                );
            }
        }

        if verbose {
            if let (Some(prev), Some(cur)) = (&entry.prev_content, &entry.content) {
                print_inline_diff(prev, cur);
            } else if let Some(cur) = &entry.content {
                for line in cur.lines() {
                    println!("│    {}", format!("+ {}", line).green());
                }
                println!("│");
            }
        }
    }

    println!("│");
    println!(
        "│  {}",
        format!(
            "{} changes across {} commits (first seen: {})",
            entries.len(),
            total_commits,
            first_seen
        )
        .dimmed()
    );
    println!("└{}", "─".repeat(60));
}

/// Compute the display label for a log entry, handling combined types like "moved + signature".
fn compute_display_label(entry: &LogEntry) -> String {
    match &entry.change_type {
        EntityChangeType::Moved => {
            if entry.old_signature.is_some() && entry.old_name.is_some() {
                "moved + renamed + signature".to_string()
            } else if entry.old_signature.is_some() {
                "moved + signature".to_string()
            } else if entry.old_name.is_some() {
                "moved + renamed".to_string()
            } else {
                "moved".to_string()
            }
        }
        other => other.label().to_string(),
    }
}

fn compute_display_label_colored(entry: &LogEntry) -> colored::ColoredString {
    match &entry.change_type {
        EntityChangeType::Moved => {
            if entry.old_signature.is_some() && entry.old_name.is_some() {
                "moved + renamed + signature".blue().to_string().normal()
            } else if entry.old_signature.is_some() {
                "moved + signature".blue().to_string().normal()
            } else if entry.old_name.is_some() {
                "moved + renamed".blue().to_string().normal()
            } else {
                "moved".blue()
            }
        }
        other => other.label_colored(),
    }
}

fn print_inline_diff(before: &str, after: &str) {
    use similar::TextDiff;

    let diff = TextDiff::from_lines(before, after);
    let mut has_changes = false;

    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete => {
                has_changes = true;
                print!("│    {}", format!("- {}", change).red());
            }
            similar::ChangeTag::Insert => {
                has_changes = true;
                print!("│    {}", format!("+ {}", change).green());
            }
            similar::ChangeTag::Equal => {} // skip unchanged lines in verbose diff
        }
    }

    if has_changes {
        println!("│");
    }
}

/// Result of matching an entity at a specific commit.
struct MatchedEntity {
    entity: SemanticEntity,
    /// Whether this was a SignatureChanged or Renamed match (vs identity-preserved).
    match_type: Option<EntityChangeType>,
    old_name: Option<String>,
    old_signature: Option<String>,
}

/// Compute Jaccard token similarity between two strings.
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a: std::collections::BTreeSet<&str> = a.split_whitespace().collect();
    let set_b: std::collections::BTreeSet<&str> = b.split_whitespace().collect();
    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    intersection as f64 / union as f64
}

/// Multi-strategy entity finder that handles overloads, signature changes, and renames.
///
/// Priority:
/// Phase A: Exact (name, signature) match
/// Phase B: SignatureChanged — same name, different signature, content similarity
/// Phase C: Renamed — different name, content/structure matches
fn find_entity_in_commit(
    entities: &[SemanticEntity],
    query_name: &str,
    query_signature: Option<&str>,
    prev_name: &str,
    prev_signature: Option<&str>,
    prev_content_hash: Option<&str>,
    prev_structural_hash: Option<&str>,
    prev_entity_content: Option<&str>,
) -> Option<MatchedEntity> {
    // Build lookup maps for Phase B/C (only needed if we have previous state)
    let content_map: HashMap<&str, &SemanticEntity> = if prev_content_hash.is_some() {
        entities.iter().map(|e| (e.content_hash.as_str(), e)).collect()
    } else {
        HashMap::new()
    };
    let struct_map: HashMap<&str, &SemanticEntity> = if prev_structural_hash.is_some() {
        entities
            .iter()
            .filter_map(|e| e.structural_hash.as_deref().map(|h| (h, e)))
            .collect()
    } else {
        HashMap::new()
    };

    // Phase A: Exact (name, signature) match
    if let Some(sig) = query_signature {
        // Exact signature match
        if let Some(ent) = entities.iter().find(|e| e.name == query_name && e.signature.as_deref() == Some(sig)) {
            return Some(MatchedEntity {
                entity: ent.clone(),
                match_type: None,
                old_name: None,
                old_signature: None,
            });
        }
    } else {
        // No signature constraint — match by name, disambiguate by content
        let by_name: Vec<&SemanticEntity> = entities.iter().filter(|e| e.name == query_name).collect();
        if by_name.len() == 1 {
            return Some(MatchedEntity {
                entity: by_name[0].clone(),
                match_type: None,
                old_name: None,
                old_signature: None,
            });
        }
        if !by_name.is_empty() {
            if let Some(pch) = prev_content_hash {
                if let Some(ent) = by_name.iter().find(|e| e.content_hash == pch) {
                    return Some(MatchedEntity {
                        entity: (*ent).clone(),
                        match_type: None,
                        old_name: None,
                        old_signature: None,
                    });
                }
            }
            if let Some(psh) = prev_structural_hash {
                if let Some(ent) = by_name.iter().find(|e| e.structural_hash.as_deref() == Some(psh)) {
                    return Some(MatchedEntity {
                        entity: (*ent).clone(),
                        match_type: None,
                        old_name: None,
                        old_signature: None,
                    });
                }
            }
            // Fallback: first by name (backward compat)
            return Some(MatchedEntity {
                entity: by_name[0].clone(),
                match_type: None,
                old_name: None,
                old_signature: None,
            });
        }
    }

    // Phase B: SignatureChanged — same name, different signature
    // 1. Try exact content/structural hash match (body unchanged, only signature changed)
    // 2. Fall back to Jaccard token similarity (body slightly changed due to param type)
    let sig_candidates: Vec<&SemanticEntity> = entities
        .iter()
        .filter(|e| e.name == query_name && e.signature.as_deref() != query_signature)
        .collect();

    if !sig_candidates.is_empty() {
        // Try exact hash match first
        if let Some(pch) = prev_content_hash {
            if let Some(ent) = sig_candidates.iter().find(|e| e.content_hash == pch) {
                return Some(MatchedEntity {
                    entity: (*ent).clone(),
                    match_type: Some(EntityChangeType::SignatureChanged),
                    old_name: None,
                    old_signature: prev_signature.map(|s| s.to_string()),
                });
            }
        }
        if let Some(psh) = prev_structural_hash {
            if let Some(ent) = sig_candidates.iter().find(|e| e.structural_hash.as_deref() == Some(psh)) {
                return Some(MatchedEntity {
                    entity: (*ent).clone(),
                    match_type: Some(EntityChangeType::SignatureChanged),
                    old_name: None,
                    old_signature: prev_signature.map(|s| s.to_string()),
                });
            }
        }
        // Fallback: Jaccard similarity with prev content
        if let Some(prev_content) = prev_entity_content {
            let best = sig_candidates
                .iter()
                .map(|e| (e, jaccard_similarity(prev_content, &e.content)))
                .filter(|(_, score)| *score >= 0.5)
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            if let Some((ent, _score)) = best {
                return Some(MatchedEntity {
                    entity: (*ent).clone(),
                    match_type: Some(EntityChangeType::SignatureChanged),
                    old_name: None,
                    old_signature: prev_signature.map(|s| s.to_string()),
                });
            }
        }
    }

    // Phase C: Renamed — different name, content/structure matches
    if prev_content_hash.is_some() || prev_structural_hash.is_some() {
        if let Some(pch) = prev_content_hash {
            if let Some(ent) = content_map.get(pch) {
                if ent.name != query_name {
                    return Some(MatchedEntity {
                        entity: (*ent).clone(),
                        match_type: Some(EntityChangeType::Renamed),
                        old_name: Some(prev_name.to_string()),
                        old_signature: None,
                    });
                }
            }
        }
        if let Some(psh) = prev_structural_hash {
            if let Some(ent) = struct_map.get(psh) {
                if ent.name != query_name {
                    return Some(MatchedEntity {
                        entity: (*ent).clone(),
                        match_type: Some(EntityChangeType::Renamed),
                        old_name: Some(prev_name.to_string()),
                        old_signature: None,
                    });
                }
            }
        }
    }

    // Phase C fallback: Jaccard similarity for renames where content hash changed
    // (e.g. method name appears in the body, causing hash to differ)
    if let Some(prev_content) = prev_entity_content {
        let rename_candidates: Vec<&SemanticEntity> = entities
            .iter()
            .filter(|e| e.name != query_name)
            .collect();

        if !rename_candidates.is_empty() {
            let best = rename_candidates
                .iter()
                .map(|e| (e, jaccard_similarity(prev_content, &e.content)))
                .filter(|(_, score)| *score >= 0.5)
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            if let Some((ent, _score)) = best {
                return Some(MatchedEntity {
                    entity: (*ent).clone(),
                    match_type: Some(EntityChangeType::Renamed),
                    old_name: Some(prev_name.to_string()),
                    old_signature: None,
                });
            }
        }
    }

    None
}

/// Find a predecessor entity: same name, different signature.
/// Used when the user queries by the NEW signature but the entity previously
/// had a different signature. Picks the candidate with matching parameter count.
fn find_predecessor<'a>(
    entities: &'a [SemanticEntity],
    query_name: &str,
    query_signature: Option<&str>,
) -> Option<&'a SemanticEntity> {
    let query_param_count = query_signature.map(|s| s.matches(',').count() + 1);

    let candidates: Vec<&SemanticEntity> = entities
        .iter()
        .filter(|e| e.name == query_name && e.signature.is_some() && e.signature.as_deref() != query_signature)
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Prefer candidate with matching parameter count
    if let Some(target_count) = query_param_count {
        if let Some(best) = candidates.iter().find(|e| {
            e.signature
                .as_ref()
                .map(|s| s.matches(',').count() + 1)
                == Some(target_count)
        }) {
            return Some(best);
        }
    }

    // Fallback: first candidate
    candidates.into_iter().next()
}

/// Cross-file entity search with overload awareness.
///
/// Returns (file_path, entity, change_type, old_name, old_signature).
fn search_entity_cross_file_v2(
    bridge: &GitBridge,
    registry: &ParserRegistry,
    sha: &str,
    query_name: &str,
    query_signature: Option<&str>,
    prev_content_hash: Option<&str>,
    prev_structural_hash: Option<&str>,
    prev_name: &str,
    prev_signature: Option<&str>,
    exclude_file: &str,
) -> Option<(String, SemanticEntity, EntityChangeType, Option<String>, Option<String>)> {
    let changed_files = bridge.get_commit_changed_files(sha).ok()?;

    // Pass 1: Exact (name, signature) in other files → Moved
    for file_path in &changed_files {
        if file_path == exclude_file {
            continue;
        }
        let content = match bridge.read_file_at_ref(sha, file_path) {
            Ok(Some(c)) => c,
            _ => continue,
        };
        let entities = registry.extract_entities(file_path, &content);
        let found = if let Some(sig) = query_signature {
            entities.into_iter().find(|e| e.name == query_name && e.signature.as_deref() == Some(sig))
        } else {
            entities.into_iter().find(|e| e.name == query_name)
        };
        if let Some(ent) = found {
            return Some((file_path.clone(), ent, EntityChangeType::Moved, None, None));
        }
    }

    // Pass 2: Same name, different signature + content/structure match → Moved + SignatureChanged
    if prev_content_hash.is_some() || prev_structural_hash.is_some() {
        for file_path in &changed_files {
            if file_path == exclude_file {
                continue;
            }
            let content = match bridge.read_file_at_ref(sha, file_path) {
                Ok(Some(c)) => c,
                _ => continue,
            };
            let entities = registry.extract_entities(file_path, &content);
            for ent in &entities {
                if ent.name != query_name {
                    continue;
                }
                let content_match = prev_content_hash.map_or(false, |pch| ent.content_hash == pch);
                let struct_match = prev_structural_hash.map_or(false, |psh| {
                    ent.structural_hash.as_deref() == Some(psh)
                });
                if content_match || struct_match {
                    return Some((
                        file_path.clone(),
                        ent.clone(),
                        EntityChangeType::Moved,
                        None,
                        prev_signature.map(|s| s.to_string()),
                    ));
                }
            }
        }
    }

    // Pass 3: Different name + content/structure match → Moved + Renamed
    if prev_content_hash.is_some() || prev_structural_hash.is_some() {
        for file_path in &changed_files {
            if file_path == exclude_file {
                continue;
            }
            let content = match bridge.read_file_at_ref(sha, file_path) {
                Ok(Some(c)) => c,
                _ => continue,
            };
            let entities = registry.extract_entities(file_path, &content);
            for ent in &entities {
                if ent.name == query_name {
                    continue;
                }
                let content_match = prev_content_hash.map_or(false, |pch| ent.content_hash == pch);
                let struct_match = prev_structural_hash.map_or(false, |psh| {
                    ent.structural_hash.as_deref() == Some(psh)
                });
                if content_match || struct_match {
                    return Some((
                        file_path.clone(),
                        ent.clone(),
                        EntityChangeType::Moved,
                        Some(prev_name.to_string()),
                        None,
                    ));
                }
            }
        }
    }

    None
}

fn print_json(
    query: &EntityQuery,
    file_path: &str,
    entity_type: &str,
    entries: &[LogEntry],
    verbose: bool,
) {
    let json_entries: Vec<_> = entries
        .iter()
        .map(|e| {
            let mut obj = serde_json::json!({
                "commit": {
                    "sha": e.short_sha,
                    "author": e.author,
                    "date": e.date,
                    "message": e.message,
                },
                "change_type": compute_display_label(e),
                "structural_change": matches!(e.change_type, EntityChangeType::ModifiedLogic | EntityChangeType::Added),
            });

            if let Some(fp) = &e.file_path {
                obj["file_path"] = serde_json::Value::String(fp.clone());
            }
            if let Some(pfp) = &e.prev_file_path {
                obj["prev_file_path"] = serde_json::Value::String(pfp.clone());
            }
            if let Some(on) = &e.old_name {
                obj["old_name"] = serde_json::Value::String(on.clone());
            }
            if let Some(os) = &e.old_signature {
                obj["old_signature"] = serde_json::Value::String(os.clone());
            }

            if verbose {
                if let Some(content) = &e.content {
                    obj["after_content"] = serde_json::Value::String(content.clone());
                }
                if let Some(prev) = &e.prev_content {
                    obj["before_content"] = serde_json::Value::String(prev.clone());
                }
            }

            obj
        })
        .collect();

    let entity_display = match &query.signature {
        Some(sig) => format!("{}{}", query.name, sig),
        None => query.name.clone(),
    };
    let output = serde_json::json!({
        "entity": entity_display,
        "file": file_path,
        "type": entity_type,
        "changes": json_entries,
    });

    println!("{}", serde_json::to_string(&output).unwrap());
}

/// Search for an entity in other files changed by a commit.
/// First tries matching by name, then falls back to structural_hash (handles renames).
enum FindResult {
    Found(String),
    Ambiguous(Vec<String>),
    NotFound,
}

fn find_entity_file(
    root: &Path,
    registry: &sem_core::parser::registry::ParserRegistry,
    query: &EntityQuery,
) -> FindResult {
    let ext_filter: Vec<String> = vec![];
    let files = super::graph::find_supported_files_public(root, registry, &ext_filter);
    let mut found_in: Vec<String> = Vec::new();

    // Pass 1: Try exact (name, signature) match
    for file_path in &files {
        let full_path = root.join(file_path);
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let entities = registry.extract_entities(file_path, &content);
        let matches: Vec<_> = if let Some(sig) = &query.signature {
            entities.iter().filter(|e| e.name == query.name && e.signature.as_deref() == Some(sig.as_str())).collect()
        } else {
            entities.iter().filter(|e| e.name == query.name).collect()
        };
        if !matches.is_empty() {
            found_in.push(file_path.clone());
        }
    }

    // Pass 2: If not found and signature was specified, fall back to name-only search.
    // The signature may have changed — find the file by name, then let the log
    // tracking algorithm detect the signature change via content matching.
    if found_in.is_empty() && query.signature.is_some() {
        for file_path in &files {
            let full_path = root.join(file_path);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let entities = registry.extract_entities(file_path, &content);
            if entities.iter().any(|e| e.name == query.name) {
                found_in.push(file_path.clone());
            }
        }
    }

    match found_in.len() {
        0 => FindResult::NotFound,
        1 => FindResult::Found(found_in.into_iter().next().unwrap()),
        _ => FindResult::Ambiguous(found_in),
    }
}

/// Simple timestamp formatting without external deps.
fn chrono_lite_format(unix_seconds: i64) -> String {
    let days = unix_seconds / 86400;
    let mut y = 1970i64;
    let mut remaining_days = days;

    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if remaining_days < year_days {
            break;
        }
        remaining_days -= year_days;
        y += 1;
    }

    let month_days = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 0;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining_days < md {
            m = i;
            break;
        }
        remaining_days -= md;
    }

    let d = remaining_days + 1;
    format!("{:04}-{:02}-{:02}", y, m + 1, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entity(name: &str, signature: Option<&str>, content_hash: &str, structural_hash: Option<&str>) -> SemanticEntity {
        SemanticEntity {
            id: format!("file.cs::method::{name}{}", signature.unwrap_or("")),
            file_path: "file.cs".to_string(),
            entity_type: "method".to_string(),
            name: name.to_string(),
            signature: signature.map(|s| s.to_string()),
            parent_id: None,
            content: String::new(),
            content_hash: content_hash.to_string(),
            structural_hash: structural_hash.map(|s| s.to_string()),
            start_line: 1,
            end_line: 10,
            metadata: None,
        }
    }

    // --- parse_entity_query tests ---

    #[test]
    fn test_parse_entity_query_name_only() {
        let q = parse_entity_query("Process");
        assert_eq!(q.name, "Process");
        assert!(q.signature.is_none());
    }

    #[test]
    fn test_parse_entity_query_with_signature() {
        let q = parse_entity_query("Process(int,string)");
        assert_eq!(q.name, "Process");
        assert_eq!(q.signature.as_deref(), Some("(int,string)"));
    }

    #[test]
    fn test_parse_entity_query_empty_params() {
        let q = parse_entity_query("Process()");
        assert_eq!(q.name, "Process");
        assert_eq!(q.signature.as_deref(), Some("()"));
    }

    #[test]
    fn test_parse_entity_query_complex_types() {
        let q = parse_entity_query("Handle(std::vector<int>,string)");
        assert_eq!(q.name, "Handle");
        assert_eq!(q.signature.as_deref(), Some("(std::vector<int>,string)"));
    }

    #[test]
    fn test_parse_entity_query_nested_parens() {
        // Name containing parentheses — should use rfind
        let q = parse_entity_query("operator()(int)");
        assert_eq!(q.name, "operator()");
        assert_eq!(q.signature.as_deref(), Some("(int)"));
    }

    // --- find_entity_in_commit tests ---

    #[test]
    fn test_find_entity_exact_overload() {
        let entities = vec![
            make_entity("Process", Some("(int)"), "hash_a", None),
            make_entity("Process", Some("(string)"), "hash_b", None),
        ];
        let result = find_entity_in_commit(
            &entities, "Process", Some("(int)"),
            "Process", Some("(int)"),
            None, None, None,
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.content_hash, "hash_a");
        assert!(m.match_type.is_none());
    }

    #[test]
    fn test_find_entity_no_signature_single() {
        let entities = vec![
            make_entity("Process", None, "hash_a", None),
        ];
        let result = find_entity_in_commit(
            &entities, "Process", None,
            "Process", None,
            None, None, None,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().entity.content_hash, "hash_a");
    }

    #[test]
    fn test_find_entity_no_signature_picks_by_content() {
        let entities = vec![
            make_entity("Process", Some("(int)"), "hash_a", None),
            make_entity("Process", Some("(string)"), "hash_b", None),
        ];
        // prev was hash_b, so should pick (string) overload
        let result = find_entity_in_commit(
            &entities, "Process", None,
            "Process", None,
            Some("hash_b"), None, None,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().entity.content_hash, "hash_b");
    }

    #[test]
    fn test_find_entity_no_signature_picks_by_structural_hash() {
        let entities = vec![
            make_entity("Process", Some("(int)"), "hash_a", Some("struct_a")),
            make_entity("Process", Some("(string)"), "hash_b", Some("struct_b")),
        ];
        let result = find_entity_in_commit(
            &entities, "Process", None,
            "Process", None,
            None, Some("struct_b"), None,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().entity.content_hash, "hash_b");
    }

    #[test]
    fn test_find_entity_signature_changed_by_content() {
        let entities = vec![
            make_entity("Process", Some("(string)"), "hash_a", None),
        ];
        // Previous was Process(int) with content_hash=hash_a
        let result = find_entity_in_commit(
            &entities, "Process", Some("(int)"),
            "Process", Some("(int)"),
            Some("hash_a"), None, None,
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.signature.as_deref(), Some("(string)"));
        assert!(matches!(m.match_type, Some(EntityChangeType::SignatureChanged)));
        assert_eq!(m.old_signature.as_deref(), Some("(int)"));
    }

    #[test]
    fn test_find_entity_signature_changed_by_structural_hash() {
        let entities = vec![
            make_entity("Process", Some("(string)"), "hash_x", Some("struct_a")),
        ];
        let result = find_entity_in_commit(
            &entities, "Process", Some("(int)"),
            "Process", Some("(int)"),
            None, Some("struct_a"), None,
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.signature.as_deref(), Some("(string)"));
        assert!(matches!(m.match_type, Some(EntityChangeType::SignatureChanged)));
    }

    #[test]
    fn test_find_entity_signature_changed_by_jaccard() {
        // Signature changed: bool → float in a parameter
        // content_hash and structural_hash both differ, but content is very similar
        let prev_content = "public int GetDownPackageCount(bool isSilent, bool hasWait, bool isHasPause)\n{\n    return 1;\n}";
        let new_content =  "public int GetDownPackageCount(bool isSilent, bool hasWait, float isHasPause)\n{\n    return 1;\n}";
        let entities = vec![
            SemanticEntity {
                id: "file.cs::method::GetDownPackageCount(bool,bool,float)".to_string(),
                file_path: "file.cs".to_string(),
                entity_type: "method".to_string(),
                name: "GetDownPackageCount".to_string(),
                signature: Some("(bool,bool,float)".to_string()),
                parent_id: None,
                content: new_content.to_string(),
                content_hash: "hash_float".to_string(),
                structural_hash: Some("struct_float".to_string()),
                start_line: 1,
                end_line: 10,
                metadata: None,
            },
            make_entity("GetDownPackageCount", Some("(bool,bool)"), "hash_short", None),
        ];
        let result = find_entity_in_commit(
            &entities, "GetDownPackageCount", Some("(bool,bool,bool)"),
            "GetDownPackageCount", Some("(bool,bool,bool)"),
            Some("hash_bool"), Some("struct_bool"),
            Some(prev_content),
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.signature.as_deref(), Some("(bool,bool,float)"));
        assert!(matches!(m.match_type, Some(EntityChangeType::SignatureChanged)));
        assert_eq!(m.old_signature.as_deref(), Some("(bool,bool,bool)"));
    }

    #[test]
    fn test_find_entity_renamed_by_content_hash() {
        let entities = vec![
            make_entity("Handle", Some("(int)"), "hash_a", None),
        ];
        // Previous was Process(int) with content_hash=hash_a
        let result = find_entity_in_commit(
            &entities, "Process", Some("(int)"),
            "Process", Some("(int)"),
            Some("hash_a"), None, None,
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.name, "Handle");
        assert!(matches!(m.match_type, Some(EntityChangeType::Renamed)));
        assert_eq!(m.old_name.as_deref(), Some("Process"));
    }

    #[test]
    fn test_find_entity_renamed_by_structural_hash() {
        let entities = vec![
            make_entity("Handle", Some("(int)"), "hash_x", Some("struct_a")),
        ];
        let result = find_entity_in_commit(
            &entities, "Process", Some("(int)"),
            "Process", Some("(int)"),
            None, Some("struct_a"), None,
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.name, "Handle");
        assert!(matches!(m.match_type, Some(EntityChangeType::Renamed)));
    }

    #[test]
    fn test_find_entity_not_found() {
        let entities = vec![
            make_entity("Other", None, "hash_a", None),
        ];
        let result = find_entity_in_commit(
            &entities, "Process", Some("(int)"),
            "Process", Some("(int)"),
            Some("hash_z"), None, None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_find_entity_signature_change_preferred_over_rename() {
        // Process(string) [sig change] matches by structural_hash, Handle(int) [rename] doesn't
        // Signature change should be preferred (Phase B before Phase C)
        let entities = vec![
            make_entity("Process", Some("(string)"), "hash_x", Some("struct_a")),
            make_entity("Handle", Some("(int)"), "hash_y", None),
        ];
        let result = find_entity_in_commit(
            &entities, "Process", Some("(int)"),
            "Process", Some("(int)"),
            None, Some("struct_a"), None,
        );
        assert!(result.is_some());
        let m = result.unwrap();
        // Should match Process(string) as SignatureChanged via structural_hash
        assert_eq!(m.entity.name, "Process");
        assert!(matches!(m.match_type, Some(EntityChangeType::SignatureChanged)));
    }
}
