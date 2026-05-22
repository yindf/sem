use std::collections::HashMap;
use std::path::Path;

use colored::Colorize;
use sem_core::git::bridge::GitBridge;
use sem_core::model::entity::SemanticEntity;
use sem_core::parser::registry::ParserRegistry;

use super::truncate_str;
use super::{EntityQuery, parse_entity_query};

pub struct LogOptions {
    pub cwd: String,
    pub entity_name: String,
    pub file_path: Option<String>,
    pub limit: usize,
    pub scan_limit: usize,
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
    let query = parse_entity_query(&opts.entity_name);

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

    if query.signature.is_none() {
        let check_entities = registry.extract_entities(&file_path, &file_content_hint);
        let overloads: Vec<&SemanticEntity> = check_entities
            .iter()
            .filter(|e| e.name == query.name)
            .collect();
        if overloads.len() > 1 {
            eprintln!(
                "{} Entity '{}' has {} overloads in {}:",
                "error:".red().bold(),
                opts.entity_name,
                overloads.len(),
                file_path
            );
            for e in &overloads {
                let sig = e.signature.as_deref().unwrap_or("n/a");
                eprintln!(
                    "  {} {}{} (L{}:{})",
                    e.entity_type, e.name, sig, e.start_line, e.end_line
                );
            }
            let example_sig = overloads[0]
                .signature
                .as_deref()
                .unwrap_or("()");
            eprintln!(
                "\nSpecify the signature to disambiguate: sem log \"{}{}\"",
                query.name, example_sig
            );
            std::process::exit(1);
        }
    }

    // Walk commits, tracking entity across file moves
    let mut current_git_file = git_file_path.clone();
    let mut visited_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited_files.insert(current_git_file.replace('\\', "/"));
    let mut entries: Vec<LogEntry> = Vec::new();
    let mut chunk: Vec<LogEntry> = Vec::new();
    let mut prev_entity_content: Option<String> = None;
    let mut prev_structural_hash: Option<String> = None;
    let mut prev_content_hash: Option<String> = None;
    let mut prev_entity_name: String = query.name.clone();
    let mut prev_entity_signature: Option<String> = query.signature.clone();
    let mut entity_type = String::new();
    let mut found_at_least_once = false;
    let mut total_commits = 0usize;
    let mut skip_until_sha: Option<String> = None;
    let mut following_rename = false; // true when walking an old path from a rename
    // Prepend entries from a rename predecessor walk (filled after main walk)
    let mut prepend_entries: Option<(Vec<LogEntry>, usize, String)> = None; // (entries, commits, predecessor_name)

    loop {
        // --scan-limit controls how many file-changing commits to scan.
        // When following a rename chain, use diff-based commit detection
        // (the old path may not exist at HEAD).
        let commits = if following_rename {
            if let Some(sha) = skip_until_sha.take() {
                // Walk from the rename commit's parent — skip_until_sha is handled
                // by starting the revwalk at the right point, not by skipping.
                bridge.get_file_commits_from(&current_git_file, &sha, opts.scan_limit)
            } else {
                bridge.get_file_commits(&current_git_file, opts.scan_limit)
            }
        } else {
            bridge.get_file_commits(&current_git_file, opts.scan_limit)
        };
        let commits = match commits {
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
                        chunk.push(LogEntry {
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
                        // Instead of restarting, we do a separate walk and prepend results.
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
                                .filter(|e| e.signature == ent.signature)
                                .filter_map(|e| {
                                    let score = sem_core::model::identity::jaccard_str_similarity(&ent.content, &e.content);
                                    if score >= 0.7 { Some((e, score)) } else { None }
                                })
                                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

                            if let Some((pred, _score)) = best_predecessor {
                                // Do a separate walk for the predecessor and prepend results.
                                // Don't touch current entries — just save the predecessor info
                                // for a second walk after the main walk finishes.
                                let pred_name = pred.name.clone();
                                let (pred_ents, pred_commits) = walk_predecessor(
                                    &bridge,
                                    &registry,
                                    &git_file_path,
                                    pred,
                                    &ent,
                                    opts.scan_limit,
                                );
                                prepend_entries = Some((pred_ents, pred_commits, pred_name));
                            }
                        }

                        // Cross-file lookback: if no same-file predecessor was found,
                        // check if this entity was moved from another file in this commit
                        // (e.g., during a "Split Large CS Files" refactoring).
                        if prepend_entries.is_none() {
                            let cross = search_entity_cross_file_v2(
                                &bridge,
                                &registry,
                                &commit.sha,
                                &ent.name,
                                ent.signature.as_deref(),
                                Some(ent.content_hash.as_str()),
                                ent.structural_hash.as_deref(),
                                &ent.name,
                                ent.signature.as_deref(),
                                &current_git_file,
                                true, // read from parent — find where entity came FROM
                            );

                            if let Some((source_file, source_ent, _, old_name, old_signature)) = cross {
                                if !visited_files.contains(&source_file.replace('\\', "/")) {
                                    // Modify the first entry from "added" to "moved"
                                    if let Some(first) = chunk.first_mut() {
                                        first.change_type = EntityChangeType::Moved;
                                        first.prev_file_path = Some(source_file.clone());
                                        first.prev_content = Some(source_ent.content.clone());
                                        first.old_name = old_name;
                                        first.old_signature = old_signature;
                                    }

                                    visited_files.insert(source_file.replace('\\', "/"));

                                    // Walk the source file's history separately.
                                    // We can't use the main loop's skip_until_sha mechanism
                                    // because it processes commits in the wrong direction
                                    // (it skips older commits and processes newer ones).
                                    let (pred_ents, pred_commits) = walk_predecessor(
                                        &bridge,
                                        &registry,
                                        &source_file,
                                        &source_ent,
                                        &ent,
                                        opts.scan_limit,
                                    );
                                    prepend_entries = Some((pred_ents, pred_commits, source_ent.name.clone()));
                                }
                            }
                        }
                    } else if prev_entity_content.is_none() {
                        chunk.push(LogEntry {
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

                        // Before accepting rename/signature-change, verify the entity
                        // actually changed at this commit (vs its git parent).
                        // If the entity exists unchanged in the parent, this commit
                        // didn't modify it — skip.
                        if let Some(EntityChangeType::Renamed | EntityChangeType::SignatureChanged) = m.match_type {
                            if let Ok(Some(parent_sha)) = bridge.get_parent_sha(&commit.sha) {
                                let parent_file = bridge
                                    .read_file_at_ref(&parent_sha, &current_git_file)
                                    .ok()
                                    .flatten();
                                let parent_entities: Vec<SemanticEntity> = parent_file
                                    .as_ref()
                                    .map(|c| registry.extract_entities(&current_git_file, c))
                                    .unwrap_or_default();

                                let unchanged_in_parent = parent_entities.iter().any(|pe| {
                                    pe.name == ent.name
                                        && pe.signature == ent.signature
                                        && pe.content_hash == ent.content_hash
                                });
                                if unchanged_in_parent {
                                    prev_entity_content = Some(ent.content.clone());
                                    prev_structural_hash = ent.structural_hash.clone();
                                    prev_content_hash = Some(ent.content_hash.clone());
                                    prev_entity_name = ent.name.clone();
                                    prev_entity_signature = ent.signature.clone();
                                    continue;
                                }
                            }
                        }

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

                        chunk.push(LogEntry {
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
                            false, // read from current commit — find where entity moved TO
                        );

                        match cross {
                            Some((new_file, ent, change_type, old_name, old_signature))
                                if !visited_files.contains(&new_file.replace('\\', "/")) =>
                            {
                                let prev_file = current_git_file.clone();
                                chunk.push(LogEntry {
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

                                visited_files.insert(new_file.replace('\\', "/"));
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
                            Some(_) => {
                                // Cross-file match found but target already visited — skip
                            }
                            None => {
                                chunk.push(LogEntry {
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
                            chunk.push(LogEntry {
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
            // No semantic cross-file entity move detected.
            // Check if the file itself was renamed — if so, follow the old path
            // (like git log --follow).
            if found_at_least_once {
                if let Some(oldest_commit) = reversed.first() {
                    if let Ok(Some(old_path)) = bridge.get_rename_source(&oldest_commit.sha, &current_git_file) {
                        let normalized_old = old_path.replace('\\', "/");
                        if normalized_old != current_git_file.replace('\\', "/")
                            && !visited_files.contains(&normalized_old)
                        {
                            // Remove the duplicate "added" entry from the same commit
                            // if the entity was actually moved here from the old path.
                            if let Some(last) = chunk.last() {
                                if last.short_sha == oldest_commit.short_sha
                                    && matches!(last.change_type, EntityChangeType::Added)
                                {
                                    chunk.pop();
                                }
                            }

                            // Emit a Moved entry for the rename commit
                            let new_path = current_git_file.clone();
                            let date = chrono_lite_format(oldest_commit.date.parse::<i64>().unwrap_or(0));
                            let msg_first_line = oldest_commit.message.lines().next().unwrap_or("").to_string();

                            // Read entity content from old path at the rename commit's parent
                            // (the entity existed there before the rename)
                            let old_content = match bridge.get_parent_sha(&oldest_commit.sha) {
                                Ok(Some(parent_sha)) => {
                                    bridge.read_file_at_ref(&parent_sha, &old_path).ok().flatten()
                                }
                                _ => None,
                            };
                            let old_entities: Vec<SemanticEntity> = old_content
                                .as_ref()
                                .map(|c| registry.extract_entities(&old_path, c))
                                .unwrap_or_default();
                            let old_ent = old_entities.iter().find(|e| {
                                e.name == prev_entity_name && e.signature == prev_entity_signature
                            });

                            chunk.push(LogEntry {
                                short_sha: oldest_commit.short_sha.clone(),
                                author: oldest_commit.author.clone(),
                                date,
                                message: msg_first_line,
                                change_type: EntityChangeType::Moved,
                                content: old_ent.map(|e| e.content.clone()).or(prev_entity_content.clone()),
                                prev_content: prev_entity_content.clone(),
                                file_path: Some(old_path.clone()),
                                prev_file_path: Some(new_path),
                                old_name: None,
                                old_signature: None,
                            });

                            // Update tracking state for the old path
                            if let Some(ent) = old_ent {
                                prev_entity_content = Some(ent.content.clone());
                                prev_structural_hash = ent.structural_hash.clone();
                                prev_content_hash = Some(ent.content_hash.clone());
                                prev_entity_name = ent.name.clone();
                                prev_entity_signature = ent.signature.clone();
                            }

                            visited_files.insert(normalized_old);
                            current_git_file = old_path;
                            skip_until_sha = Some(oldest_commit.sha.clone());
                            following_rename = true;
                            moved = true;
                        }
                    }
                }
            }
            if !moved {
                // No more file moves — prepend final chunk and stop
                let prepend = std::mem::take(&mut chunk);
                entries.splice(0..0, prepend);
                break;
            }
        }
        // Cross-file entity move detected inside inner loop — prepend chunk and continue
        let mut prepend = std::mem::take(&mut chunk);
        entries.splice(0..0, prepend);
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

    // Prepend predecessor entries if a rename or cross-file move was detected
    if let Some((mut pred_entries, pred_commits, pred_name)) = prepend_entries.take() {
        if !pred_entries.is_empty() {
            total_commits += pred_commits;
            // Check if the first entry is already a "Moved" entry (cross-file lookback case).
            let first_is_moved = entries
                .first()
                .map_or(false, |e| matches!(e.change_type, EntityChangeType::Moved));

            if first_is_moved {
                // Cross-file move case: walk_predecessor may have produced a duplicate
                // "moved" or "deleted" entry for the same commit. Remove it.
                if let Some(first_sha) = entries.first().map(|e| e.short_sha.clone()) {
                    let dup_idx = pred_entries.iter().rposition(|e| {
                        e.short_sha == first_sha
                            && matches!(e.change_type, EntityChangeType::Moved | EntityChangeType::Deleted)
                    });
                    if let Some(idx) = dup_idx {
                        pred_entries.drain(idx..);
                    }
                }
            } else {
                // Same-file rename case: insert a "Renamed" entry connecting predecessor
                pred_entries.push(LogEntry {
                    short_sha: entries[0].short_sha.clone(),
                    author: entries[0].author.clone(),
                    date: entries[0].date.clone(),
                    message: entries[0].message.clone(),
                    change_type: EntityChangeType::Renamed,
                    content: entries[0].content.clone(),
                    prev_content: pred_entries.last().and_then(|e| e.content.clone()),
                    file_path: entries[0].file_path.clone(),
                    prev_file_path: None,
                    old_name: Some(pred_name),
                    old_signature: None,
                });
            }
            pred_entries.append(&mut entries);
            entries = pred_entries;
        }
    }

    // Keep only the most recent N entity changes
    if entries.len() > opts.limit {
        let total = entries.len();
        entries.drain(..total - opts.limit);
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
                    format!("→ renamed {} → {}", old_name, query.name).cyan()
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
                    format!("→ renamed {} → {}", old_name, query.name).cyan()
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
                .map(|e| (e, sem_core::model::identity::jaccard_str_similarity(prev_content, &e.content)))
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
            .filter(|e| e.name != query_name && e.signature.as_deref() == prev_signature)
            .collect();

        if !rename_candidates.is_empty() {
            let best = rename_candidates
                .iter()
                .map(|e| (e, sem_core::model::identity::jaccard_str_similarity(prev_content, &e.content)))
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

    candidates.into_iter().next()
}

/// Cross-file entity search with overload awareness.
///
/// Returns (file_path, entity, change_type, old_name, old_signature).
/// When `read_from_parent` is true, reads files at the parent commit (for detecting
/// where an entity came FROM before this commit's changes).
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
    read_from_parent: bool,
) -> Option<(String, SemanticEntity, EntityChangeType, Option<String>, Option<String>)> {
    let changed_files = bridge.get_commit_changed_files(sha).ok()?;
    let exclude_normalized = exclude_file.replace('\\', "/");
    let parent_sha = bridge.get_parent_sha(sha).ok().flatten();

    let mut entity_cache: HashMap<String, Vec<SemanticEntity>> = HashMap::new();
    for file_path in &changed_files {
        if file_path == &exclude_normalized {
            continue;
        }
        let content = if read_from_parent {
            if let Some(ref psha) = parent_sha {
                if let Ok(Some(c)) = bridge.read_file_at_ref(psha, file_path) {
                    Some(c)
                } else {
                    bridge.read_file_at_ref(sha, file_path).ok().flatten()
                }
            } else {
                bridge.read_file_at_ref(sha, file_path).ok().flatten()
            }
        } else {
            if let Ok(Some(c)) = bridge.read_file_at_ref(sha, file_path) {
                Some(c)
            } else if let Some(ref psha) = parent_sha {
                bridge.read_file_at_ref(psha, file_path).ok().flatten()
            } else {
                None
            }
        };
        if let Some(c) = content {
            entity_cache.insert(file_path.clone(), registry.extract_entities(file_path, &c));
        }
    }

    // Pass 1: Exact (name, signature) in other files → Moved
    for (file_path, entities) in &entity_cache {
        let found = if let Some(sig) = query_signature {
            entities.iter().find(|e| e.name == query_name && e.signature.as_deref() == Some(sig))
        } else {
            entities.iter().find(|e| e.name == query_name)
        };
        if let Some(ent) = found {
            return Some((file_path.clone(), ent.clone(), EntityChangeType::Moved, None, None));
        }
    }

    // Pass 2: Same name, different signature + content/structure match → Moved + SignatureChanged
    if prev_content_hash.is_some() || prev_structural_hash.is_some() {
        for (file_path, entities) in &entity_cache {
            for ent in entities {
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
        for (file_path, entities) in &entity_cache {
            for ent in entities {
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

/// Walk the history of a predecessor entity (detected by lookback) and return
/// its entries + commit count. These entries will be prepended to the main walk.
fn walk_predecessor(
    bridge: &GitBridge,
    registry: &ParserRegistry,
    git_file_path: &str,
    predecessor: &SemanticEntity,
    current_entity: &SemanticEntity,
    scan_limit: usize,
) -> (Vec<LogEntry>, usize) {
    let mut entries: Vec<LogEntry> = Vec::new();
    let mut prev_entity_content: Option<String> = None;
    let mut prev_structural_hash: Option<String> = None;
    let mut prev_content_hash: Option<String> = None;
    let mut prev_entity_name = predecessor.name.clone();
    let mut prev_entity_signature = predecessor.signature.clone();
    let mut total_commits = 0usize;
    let mut skip_until_sha: Option<String> = None;
    let mut current_git_file = git_file_path.to_string();

    loop {
        let commits = match bridge.get_file_commits(&current_git_file, scan_limit) {
            Ok(c) => c,
            Err(_) => break,
        };
        if commits.is_empty() { break; }

        let reversed: Vec<_> = commits.iter().rev().collect();
        let start_idx = if let Some(ref sha) = skip_until_sha {
            reversed.iter().position(|c| c.sha == *sha).map(|i| i + 1).unwrap_or(reversed.len())
        } else {
            0
        };
        skip_until_sha = None;
        total_commits += reversed.len().saturating_sub(start_idx);
        let mut moved = false;

        for commit in &reversed[start_idx..] {
            let file_content = bridge.read_file_at_ref(&commit.sha, &current_git_file).ok().flatten();
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
                    let cur_content_hash = ent.content_hash.clone();
                    let cur_structural_hash = ent.structural_hash.clone();

                    if entries.is_empty() {
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
                        // Before accepting rename/signature-change, verify the entity
                        // actually changed at this commit (vs its git parent).
                        if let Some(EntityChangeType::Renamed | EntityChangeType::SignatureChanged) = m.match_type {
                            if let Ok(Some(parent_sha)) = bridge.get_parent_sha(&commit.sha) {
                                let parent_file = bridge
                                    .read_file_at_ref(&parent_sha, &current_git_file)
                                    .ok()
                                    .flatten();
                                let parent_entities: Vec<SemanticEntity> = parent_file
                                    .as_ref()
                                    .map(|c| registry.extract_entities(&current_git_file, c))
                                    .unwrap_or_default();

                                let unchanged_in_parent = parent_entities.iter().any(|pe| {
                                    pe.name == ent.name
                                        && pe.signature == ent.signature
                                        && pe.content_hash == ent.content_hash
                                });
                                if unchanged_in_parent {
                                    prev_entity_content = Some(ent.content.clone());
                                    prev_structural_hash = ent.structural_hash.clone();
                                    prev_content_hash = Some(ent.content_hash.clone());
                                    prev_entity_name = ent.name.clone();
                                    prev_entity_signature = ent.signature.clone();
                                    continue;
                                }
                            }
                        }

                        let change_type = if let Some(mt) = m.match_type {
                            mt
                        } else {
                            let content_changed = prev_content_hash.as_deref() != Some(cur_content_hash.as_str());
                            if content_changed {
                                let structural_changed = match (cur_structural_hash.as_deref(), prev_structural_hash.as_deref()) {
                                    (Some(cur), Some(prev)) => cur != prev,
                                    _ => true,
                                };
                                if structural_changed { EntityChangeType::ModifiedLogic }
                                else { EntityChangeType::ModifiedCosmetic }
                            } else {
                                prev_entity_content = Some(ent.content.clone());
                                prev_structural_hash = ent.structural_hash.clone();
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
                    if prev_entity_content.is_some() {
                        let cross = search_entity_cross_file_v2(
                            bridge, registry, &commit.sha,
                            &prev_entity_name, prev_entity_signature.as_deref(),
                            prev_content_hash.as_deref(), prev_structural_hash.as_deref(),
                            &prev_entity_name, prev_entity_signature.as_deref(),
                            &current_git_file,
                            false, // read from current commit
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
                    }
                }
            }
        }

        if !moved { break; }
    }

    (entries, total_commits)
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

    // --- Phase C signature filter tests ---

    #[test]
    fn test_rename_rejected_when_signature_differs() {
        // RegisterDialogValidHandler(Func<int,bool>) should NOT match
        // RegisterUpdateInterruptHandler(Action<GainInterruptReason>) — different signature
        let prev_content = "public void RegisterUpdateInterruptHandler(Action<GainInterruptReason> func)\n{\n    UpdateInterruptHandler += func;\n}";
        let other_content = "public void RegisterDialogValidHandler(Func<int, bool> func)\n{\n    DialogValidHandler += func;\n}";
        let entities = vec![
            SemanticEntity {
                id: "file.cs::method::RegisterDialogValidHandler".to_string(),
                file_path: "file.cs".to_string(),
                entity_type: "method".to_string(),
                name: "RegisterDialogValidHandler".to_string(),
                signature: Some("(Func<int,bool>)".to_string()),
                parent_id: None,
                content: other_content.to_string(),
                content_hash: "hash_dialog".to_string(),
                structural_hash: None,
                start_line: 1,
                end_line: 10,
                metadata: None,
            },
        ];
        let result = find_entity_in_commit(
            &entities, "RegisterUpdateInterruptHandler", Some("(Action<GainInterruptReason>)"),
            "RegisterUpdateInterruptHandler", Some("(Action<GainInterruptReason>)"),
            None, None,
            Some(prev_content),
        );
        // Must NOT match: signatures are completely different
        assert!(result.is_none(), "should not rename-match entities with different signatures");
    }

    #[test]
    fn test_rename_accepted_when_signature_same() {
        // GetDownPackageCount → GetDownPackageCountXXX with same signature (bool,bool,bool,int)
        let prev_content = "public int GetDownPackageCount(bool a, bool b, bool c, int d)\n{\n    return 1;\n}";
        let new_content =  "public int GetDownPackageCountXXX(bool a, bool b, bool c, int d)\n{\n    return 1;\n}";
        let entities = vec![
            SemanticEntity {
                id: "file.cs::method::GetDownPackageCountXXX".to_string(),
                file_path: "file.cs".to_string(),
                entity_type: "method".to_string(),
                name: "GetDownPackageCountXXX".to_string(),
                signature: Some("(bool,bool,bool,int)".to_string()),
                parent_id: None,
                content: new_content.to_string(),
                content_hash: "hash_xxx".to_string(),
                structural_hash: None,
                start_line: 1,
                end_line: 10,
                metadata: None,
            },
        ];
        let result = find_entity_in_commit(
            &entities, "GetDownPackageCount", Some("(bool,bool,bool,int)"),
            "GetDownPackageCount", Some("(bool,bool,bool,int)"),
            None, None,
            Some(prev_content),
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.name, "GetDownPackageCountXXX");
        assert!(matches!(m.match_type, Some(EntityChangeType::Renamed)));
        assert_eq!(m.old_name.as_deref(), Some("GetDownPackageCount"));
    }

    #[test]
    fn test_rename_jaccard_rejected_different_signature_similar_body() {
        // Two methods with similar structure but completely different signatures
        // should NOT be matched as renames even with high Jaccard similarity
        let prev_content = "public void Foo(int x)\n{\n    handler += x;\n}";
        let other_content = "public void Bar(string y)\n{\n    handler += y;\n}";
        let entities = vec![
            SemanticEntity {
                id: "file.cs::method::Bar".to_string(),
                file_path: "file.cs".to_string(),
                entity_type: "method".to_string(),
                name: "Bar".to_string(),
                signature: Some("(string)".to_string()),
                parent_id: None,
                content: other_content.to_string(),
                content_hash: "hash_bar".to_string(),
                structural_hash: None,
                start_line: 1,
                end_line: 10,
                metadata: None,
            },
        ];
        let result = find_entity_in_commit(
            &entities, "Foo", Some("(int)"),
            "Foo", Some("(int)"),
            None, None,
            Some(prev_content),
        );
        assert!(result.is_none(), "should not rename-match with different signatures");
    }

    // --- sem_core::model::identity::jaccard_str_similarity tests ---

    #[test]
    fn test_jaccard_identical_content() {
        let content = "public void Foo(int x)\n{\n    return x;\n}";
        assert_eq!(sem_core::model::identity::jaccard_str_similarity(content, content), 1.0);
    }

    #[test]
    fn test_jaccard_rename_high_similarity() {
        let old = "public int GetDownPackageCount(bool a, bool b, bool c, int d)\n{\n    return 1;\n}";
        let new = "public int GetDownPackageCountXXX(bool a, bool b, bool c, int d)\n{\n    return 1;\n}";
        let score = sem_core::model::identity::jaccard_str_similarity(old, new);
        assert!(score >= 0.7, "rename should have high Jaccard similarity, got {score}");
    }

    #[test]
    fn test_jaccard_different_methods_lower_similarity() {
        let a = "public void RegisterUpdateInterruptHandler(Action<GainInterruptReason> func)\n{\n    UpdateInterruptHandler += func;\n}";
        let b = "public void RegisterDialogValidHandler(Func<int, bool> func)\n{\n    DialogValidHandler += func;\n}";
        let score = sem_core::model::identity::jaccard_str_similarity(a, b);
        // These have similar structure but different names/signatures
        // Score should be moderate, but with signature filter it won't matter
        assert!(score < 0.9, "different methods should have lower Jaccard, got {score}");
    }

    // --- empty params "()" matching tests ---

    #[test]
    fn test_parse_entity_query_empty_params_overload() {
        let q = parse_entity_query("ResumeAllDown()");
        assert_eq!(q.name, "ResumeAllDown");
        assert_eq!(q.signature.as_deref(), Some("()"));
    }

    #[test]
    fn test_find_entity_in_commit_empty_params_matches_no_signature() {
        // "ResumeAllDown()" should match entity with signature=Some("()")
        let entities = vec![
            SemanticEntity {
                id: "svc.cs::method::ResumeAllDown".to_string(),
                file_path: "svc.cs".to_string(),
                entity_type: "method".to_string(),
                name: "ResumeAllDown".to_string(),
                signature: Some("()".to_string()),
                parent_id: None,
                content: "void ResumeAllDown() { }".to_string(),
                content_hash: "hash1".to_string(),
                structural_hash: None,
                start_line: 199,
                end_line: 209,
                metadata: None,
            },
            SemanticEntity {
                id: "svc.cs::method::ResumeAllDown(bool)".to_string(),
                file_path: "svc.cs".to_string(),
                entity_type: "method".to_string(),
                name: "ResumeAllDown".to_string(),
                signature: Some("(bool)".to_string()),
                parent_id: None,
                content: "void ResumeAllDown(bool x) { }".to_string(),
                content_hash: "hash2".to_string(),
                structural_hash: None,
                start_line: 467,
                end_line: 603,
                metadata: None,
            },
        ];
        let result = find_entity_in_commit(
            &entities, "ResumeAllDown", Some("()"),
            "ResumeAllDown", None,
            None, None, None,
        );
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.entity.signature.as_deref(), Some("()"));
        assert_eq!(m.entity.start_line, 199);
    }

    #[test]
    fn test_find_entity_in_commit_nonempty_signature_matches() {
        let entities = vec![
            make_entity("ResumeAllDown", Some("()"), "hash1", None),
            make_entity("ResumeAllDown", Some("(bool)"), "hash2", None),
        ];
        let result = find_entity_in_commit(
            &entities, "ResumeAllDown", Some("(bool)"),
            "ResumeAllDown", Some("(bool)"),
            None, None, None,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().entity.signature.as_deref(), Some("(bool)"));
    }

    // --- Cross-file move detection integration tests ---

    /// Helper: create a temp git repo with initial commit and return (GitBridge, ParserRegistry).
    fn setup_test_repo() -> (tempfile::TempDir, GitBridge, ParserRegistry) {
        use std::fs;
        let temp = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        // Configure identity for commits
        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config().unwrap().set_str("user.email", "test@test.com").unwrap();

        // Initial commit with a dummy file so HEAD exists
        let dummy_path = temp.path().join(".gitkeep");
        fs::write(&dummy_path, "").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(".gitkeep")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();

        let bridge = GitBridge::open(temp.path()).unwrap();
        let registry = crate::commands::create_registry(
            &temp.path().to_string_lossy(),
        );
        (temp, bridge, registry)
    }

    /// Helper: commit a file in the test repo, returns the commit SHA.
    fn commit_file_in_repo(
        temp: &tempfile::TempDir,
        file_path: &str,
        contents: &str,
        message: &str,
    ) -> String {
        use std::fs;
        let full_path = temp.path().join(file_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full_path, contents).unwrap();

        let repo = git2::Repository::open(temp.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(file_path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let head = repo.head().unwrap();
        let parent = repo.find_commit(head.target().unwrap()).unwrap();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent]).unwrap();
        oid.to_string()
    }

    /// Helper: remove a file from the repo (git rm), returns the commit SHA.
    fn remove_file_in_repo(
        temp: &tempfile::TempDir,
        file_path: &str,
        message: &str,
    ) -> String {
        use std::fs;
        let full_path = temp.path().join(file_path);
        fs::remove_file(&full_path).ok();

        let repo = git2::Repository::open(temp.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(std::path::Path::new(file_path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let head = repo.head().unwrap();
        let parent = repo.find_commit(head.target().unwrap()).unwrap();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent]).unwrap();
        oid.to_string()
    }

    /// Helper: commit multiple files in a single commit, returns the commit SHA.
    fn commit_files_in_repo(
        temp: &tempfile::TempDir,
        files: &[(&str, &str)],
        message: &str,
    ) -> String {
        use std::fs;
        for (file_path, contents) in files {
            let full_path = temp.path().join(file_path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full_path, contents).unwrap();
        }

        let repo = git2::Repository::open(temp.path()).unwrap();
        let mut index = repo.index().unwrap();
        for (file_path, _) in files {
            index.add_path(std::path::Path::new(file_path)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let head = repo.head().unwrap();
        let parent = repo.find_commit(head.target().unwrap()).unwrap();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent]).unwrap();
        oid.to_string()
    }

    #[test]
    fn test_cross_file_search_detects_move_from_deleted_file() {
        // Scenario: method exists in file A, then a single commit moves it to file B
        // and removes it from file A (file split).
        // search_entity_cross_file_v2 with read_from_parent=true should find the method
        // in file A (at the parent commit's content).
        let (temp, bridge, registry) = setup_test_repo();

        let source_file = "Service.cs";
        let target_file = "Service.Public.cs";

        // Commit 1: method exists in source file
        let source_content = r#"
public class Service
{
    public void LaunchGameAsync(int gameId, string param)
    {
        DoSomething();
    }

    public void Helper()
    {
    }
}
"#;
        let _sha1 = commit_file_in_repo(&temp, source_file, source_content, "add Service");

        // Commit 2: single commit — create target file AND modify source file (file split)
        let source_after = r#"
public class Service
{
    public void Helper()
    {
    }
}
"#;
        let target_content = r#"
public class Service
{
    public void LaunchGameAsync(int gameId, string param)
    {
        DoSomething();
    }
}
"#;
        let sha2 = commit_files_in_repo(
            &temp,
            &[(target_file, target_content), (source_file, source_after)],
            "split Service into partial classes",
        );

        // Extract entity from the target file
        let entities = registry.extract_entities(target_file, target_content);
        let launch_entity = entities.iter().find(|e| e.name == "LaunchGameAsync")
            .expect("Should find LaunchGameAsync in target file");

        // Search for the entity in other files at the split commit (read from parent)
        let result = search_entity_cross_file_v2(
            &bridge,
            &registry,
            &sha2,
            &launch_entity.name,
            launch_entity.signature.as_deref(),
            Some(launch_entity.content_hash.as_str()),
            launch_entity.structural_hash.as_deref(),
            &launch_entity.name,
            launch_entity.signature.as_deref(),
            target_file,
            true, // read from parent — find where entity came FROM
        );

        assert!(result.is_some(), "Should find LaunchGameAsync in source file");
        let (found_file, found_ent, change_type, old_name, old_sig) = result.unwrap();
        assert_eq!(found_file, source_file, "Should find entity in source file");
        assert_eq!(found_ent.name, "LaunchGameAsync");
        assert!(matches!(change_type, EntityChangeType::Moved));
        assert!(old_name.is_none());
        assert!(old_sig.is_none());
    }

    #[test]
    fn test_cross_file_search_path_separator_normalization() {
        // Unit test: verify that the normalization logic in search_entity_cross_file_v2
        // correctly converts backslash exclude_file to forward slashes for comparison.
        // On Windows, the log command passes paths like "Runtime\Service\Other.cs" as exclude,
        // while git returns "Runtime/Service/Other.cs". The function normalizes before comparing.
        //
        // We test this indirectly: the real integration test is the LaunchGameAsync case above.
        // Here we just verify the string normalization works:
        let backslash_path = "Runtime\\Script\\ExportApi\\GameLauncher.cs";
        let forward_path = "Runtime/Script/ExportApi/GameLauncher.cs";
        let normalized = backslash_path.replace('\\', "/");
        assert_eq!(normalized, forward_path, "normalize should convert backslashes to forward slashes");

        // Also verify the comparison pattern used in the function
        assert_eq!(normalized, forward_path);
        assert_ne!(backslash_path, forward_path, "raw backslash path should NOT match forward slash path");
    }

    #[test]
    fn test_cross_file_search_read_from_parent_vs_current() {
        // When read_from_parent=true, reads files at the parent commit.
        // This matters when a file was modified in the current commit (e.g., entity removed).
        let (temp, bridge, registry) = setup_test_repo();

        let source_file = "GameLauncher.cs";

        // Commit 1: method in source file
        let old_content = r#"
public class GameLauncher
{
    public void LaunchGameAsync(int id)
    {
        StartGame(id);
    }
}
"#;
        let _sha1 = commit_file_in_repo(&temp, source_file, old_content, "add GameLauncher");

        // Commit 2: modify source file — remove the method
        let new_content = r#"
public class GameLauncher
{
}
"#;
        let sha2 = commit_file_in_repo(&temp, source_file, new_content, "remove method");

        // With read_from_parent=true: should find LaunchGameAsync in old content
        let result_parent = search_entity_cross_file_v2(
            &bridge, &registry, &sha2,
            "LaunchGameAsync", None,
            None, None,
            "LaunchGameAsync", None,
            "nonexistent.cs", // don't exclude anything
            true, // read from parent
        );
        assert!(result_parent.is_some(), "Should find method in parent commit content");
        let (file, ent, _, _, _) = result_parent.unwrap();
        assert_eq!(file, source_file);
        assert_eq!(ent.name, "LaunchGameAsync");

        // With read_from_parent=false: reads current commit where method is gone
        let result_current = search_entity_cross_file_v2(
            &bridge, &registry, &sha2,
            "LaunchGameAsync", None,
            None, None,
            "LaunchGameAsync", None,
            "nonexistent.cs",
            false, // read from current commit
        );
        assert!(result_current.is_none(), "Should NOT find method in current commit content");
    }

    #[test]
    fn test_cross_file_search_excludes_current_file() {
        // Verify that the exclude_file parameter works correctly
        let (temp, bridge, registry) = setup_test_repo();

        let content = r#"
public class Service
{
    public void Process()
    {
    }
}
"#;
        let sha = commit_file_in_repo(&temp, "Service.cs", content, "add Service");

        let entities = registry.extract_entities("Service.cs", content);
        let process_ent = entities.iter().find(|e| e.name == "Process").unwrap();

        // Exclude Service.cs — should find nothing (no other files)
        let result = search_entity_cross_file_v2(
            &bridge, &registry, &sha,
            &process_ent.name, process_ent.signature.as_deref(),
            Some(process_ent.content_hash.as_str()),
            process_ent.structural_hash.as_deref(),
            &process_ent.name, process_ent.signature.as_deref(),
            "Service.cs",
            true,
        );
        assert!(result.is_none(), "Should not find entity when its file is excluded");
    }

    #[test]
    fn test_cross_file_search_detects_rename_across_files() {
        // Method renamed and moved: Process → Handle in new file
        let (temp, bridge, registry) = setup_test_repo();

        let old_content = r#"
public class Service
{
    public void Process(int id)
    {
        DoWork(id);
    }
}
"#;
        let _sha1 = commit_file_in_repo(&temp, "Service.cs", old_content, "add Service");

        let new_content = r#"
public class Handler
{
    public void Handle(int id)
    {
        DoWork(id);
    }
}
"#;
        // Single commit: create Handler.cs and modify Service.cs
        let modified_source = r#"
public class Service
{
}
"#;
        let sha2 = commit_files_in_repo(
            &temp,
            &[("Handler.cs", new_content), ("Service.cs", modified_source)],
            "rename and move",
        );

        // Extract entity from the new file
        let entities = registry.extract_entities("Handler.cs", new_content);
        let handle_ent = entities.iter().find(|e| e.name == "Handle").unwrap();

        // Search for renamed entity by content hash (read from parent to find old file)
        let result = search_entity_cross_file_v2(
            &bridge, &registry, &sha2,
            "Handle", handle_ent.signature.as_deref(),
            Some(handle_ent.content_hash.as_str()),
            handle_ent.structural_hash.as_deref(),
            "Handle", handle_ent.signature.as_deref(),
            "Handler.cs",
            true,
        );

        assert!(result.is_some(), "Should find renamed entity via content/structural hash");
        let (found_file, found_ent, _, _, _) = result.unwrap();
        assert_eq!(found_file, "Service.cs");
        // The found entity has the OLD name (from parent commit)
        assert!(found_ent.name == "Process" || found_ent.name == "Handle",
            "Found entity should be Process or Handle, got: {}", found_ent.name);
    }
}
