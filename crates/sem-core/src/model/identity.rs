use std::collections::{HashMap, HashSet};

use super::change::{ChangeType, SemanticChange};
use super::entity::{SemanticEntity, logical_key};

pub fn parent_name(
    entity: &SemanticEntity,
    by_id: &HashMap<&str, &SemanticEntity>,
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut pid = entity.parent_id.as_deref()?;
    loop {
        if !visited.insert(pid) {
            break;
        }
        match by_id.get(pid) {
            Some(parent) => {
                // Skip ancestors with empty names (e.g. JSON's empty-string
                // root-package key in package-lock.json). The full path is
                // still recoverable from entity_id; the displayed chain is
                // for human readability.
                if !parent.name.is_empty() {
                    parts.push(parent.name.as_str());
                }
                match parent.parent_id.as_deref() {
                    Some(next) => pid = next,
                    None => break,
                }
            }
            None => {
                // Fallback: parent entity not in by_id (e.g. in a different
                // file for partial classes). Extract the name from the ID.
                // ID format: "file.cs::entity_type::Name" or "parent_id::Name(sig)"
                if let Some(name) = extract_name_from_id(pid) {
                    if !name.is_empty() {
                        parts.push(name);
                    }
                }
                break;
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    Some(parts.join("."))
}

/// Extract the entity name from an entity ID string.
/// "file.cs::class::AccountService" → "AccountService"
/// "file.cs::class::AccountService::GetLogoutData()" → "GetLogoutData"
fn extract_name_from_id(id: &str) -> Option<&str> {
    let last = id.rsplit("::").next()?;
    // Strip signature part (parentheses) for display name
    Some(last.split('(').next()?.trim())
}

pub struct MatchResult {
    pub changes: Vec<SemanticChange>,
}

fn classify_match(before: &SemanticEntity, after: &SemanticEntity) -> ChangeType {
    if before.file_path != after.file_path {
        ChangeType::Moved
    } else if before.parent_id != after.parent_id {
        ChangeType::Moved // intra-file scope move (e.g. method moved between classes)
    } else {
        ChangeType::Renamed
    }
}

fn same_signature_across_file_rename(
    before: &SemanticEntity,
    after: &SemanticEntity,
    before_by_id: &HashMap<&str, &SemanticEntity>,
    after_by_id: &HashMap<&str, &SemanticEntity>,
) -> bool {
    before.file_path != after.file_path
        && before.entity_type == after.entity_type
        && before.name == after.name
        && parent_name(before, before_by_id) == parent_name(after, after_by_id)
}

fn make_change(
    after_entity: &SemanticEntity,
    change_type: ChangeType,
    before_entity: Option<&SemanticEntity>,
    commit_sha: Option<&str>,
    author: Option<&str>,
    by_id: &HashMap<&str, &SemanticEntity>,
) -> SemanticChange {
    let prefix = match change_type {
        ChangeType::Added => "added::",
        ChangeType::Deleted => "deleted::",
        ChangeType::Reordered => "reordered::",
        _ => "",
    };
    // For deleted entities, use the before entity as the primary source
    let primary = if change_type == ChangeType::Deleted {
        before_entity.unwrap_or(after_entity)
    } else {
        after_entity
    };
    SemanticChange {
        id: format!("change::{prefix}{}", primary.id),
        entity_id: primary.id.clone(),
        change_type,
        entity_type: primary.entity_type.clone(),
        entity_name: primary.name.clone(),
        entity_line: primary.start_line,
        parent_name: parent_name(primary, by_id),
        file_path: primary.file_path.clone(),
        old_entity_name: before_entity.and_then(|b| {
            (b.name != after_entity.name).then(|| b.name.clone())
        }),
        signature: primary.signature.clone(),
        old_signature: before_entity.and_then(|b| {
            (b.signature != after_entity.signature).then(|| b.signature.clone()).flatten()
        }),
        old_file_path: before_entity.and_then(|b| {
            (b.file_path != after_entity.file_path).then(|| b.file_path.clone())
        }),
        old_parent_id: before_entity.and_then(|b| {
            (b.parent_id != after_entity.parent_id).then(|| b.parent_id.clone()).flatten()
        }),
        before_content: before_entity.map(|b| b.content.clone()),
        after_content: if change_type == ChangeType::Deleted || change_type == ChangeType::Reordered {
            None
        } else {
            Some(after_entity.content.clone())
        },
        commit_sha: commit_sha.map(String::from),
        author: author.map(String::from),
        timestamp: None,
        structural_change: None,
    }
}

/// 4-phase entity matching algorithm:
/// 1. Exact ID match — same entity ID in before/after → modified or unchanged
/// 1.5. Logical key match — same (file, parent, name) but different signature → signature changed
/// 2. Content hash match — same hash, different ID → renamed or moved
/// 3. Same signature across file rename → moved, even if content changed
/// 4. Fuzzy similarity — >80% content similarity → probable rename
pub fn match_entities(
    before: &[SemanticEntity],
    after: &[SemanticEntity],
    _file_path: &str,
    similarity_fn: Option<&dyn Fn(&SemanticEntity, &SemanticEntity) -> f64>,
    commit_sha: Option<&str>,
    author: Option<&str>,
) -> MatchResult {
    let mut changes: Vec<SemanticChange> = Vec::new();
    let mut matched_before: HashSet<&str> = HashSet::new();
    let mut matched_after: HashSet<&str> = HashSet::new();

    let before_by_id: HashMap<&str, &SemanticEntity> =
        before.iter().map(|e| (e.id.as_str(), e)).collect();
    let after_by_id: HashMap<&str, &SemanticEntity> =
        after.iter().map(|e| (e.id.as_str(), e)).collect();

    // Combined map for ancestor-chain lookup: after takes precedence so the
    // displayed path reflects the post-change tree for non-deleted entities.
    let combined_by_id: HashMap<&str, &SemanticEntity> = before
        .iter()
        .map(|e| (e.id.as_str(), e))
        .chain(after.iter().map(|e| (e.id.as_str(), e)))
        .collect();

    // Phase 1: Exact ID match
    for (&id, after_entity) in &after_by_id {
        if let Some(before_entity) = before_by_id.get(id) {
            matched_before.insert(id);
            matched_after.insert(id);

            if before_entity.content_hash != after_entity.content_hash {
                let mut change = make_change(after_entity, ChangeType::Modified, Some(before_entity), commit_sha, author, &combined_by_id);
                change.structural_change = match (&before_entity.structural_hash, &after_entity.structural_hash) {
                    (Some(before_sh), Some(after_sh)) => Some(before_sh != after_sh),
                    _ => None,
                };
                changes.push(change);
            }
        }
    }

    // Collect unmatched
    let unmatched_before: Vec<&SemanticEntity> = before
        .iter()
        .filter(|e| !matched_before.contains(e.id.as_str()))
        .collect();
    let _unmatched_after: Vec<&SemanticEntity> = after
        .iter()
        .filter(|e| !matched_after.contains(e.id.as_str()))
        .collect();

    // Phase 1.5: Overload signature matching with content-based optimal pairing.
    //
    // Groups ALL entities (both Phase 1 matched and unmatched) by logical key
    // and finds the optimal pairing using content similarity. This allows
    // breaking a Phase 1 "Modified" match when a cross-signature pairing
    // produces a better content match.
    //
    // Example where Phase 1 is suboptimal:
    //   Before: Process(bool,bool){simple}, Process(bool,bool,bool){complex}
    //   After:  Process(bool,bool){complex}, Process(bool,bool,float){simple}
    //   Phase 1 matches (bool,bool)→(bool,bool) as Modified.
    //   Phase 1.5 reassigns: (bool,bool,bool){complex}→(bool,bool){complex}=SignatureChanged
    //                        (bool,bool){simple}→(bool,bool,float){simple}=SignatureChanged
    {
        // Step 1: Check if any Phase 1 "Modified" match should be broken
        //         because an unmatched entity is a better content fit.
        let mut changes_to_remove: Vec<usize> = Vec::new();
        let mut new_changes: Vec<SemanticChange> = Vec::new();

        for (change_idx, change) in changes.iter().enumerate() {
            if change.change_type != ChangeType::Modified {
                continue;
            }
            if change.entity_id.is_empty() {
                continue;
            }

            // Look up the after entity for this Modified change
            let after_entity = match after_by_id.get(change.entity_id.as_str()) {
                Some(e) => *e,
                None => continue,
            };
            // Only consider entities that have signatures (overload candidates)
            if after_entity.signature.is_none() {
                continue;
            }

            let after_key = logical_key(after_entity);

            // Current Phase 1 match score
            let before_entity = match before_by_id.get(change.entity_id.as_str()) {
                Some(e) => *e,
                None => continue,
            };
            let current_score = overload_similarity(before_entity, after_entity);

            // Find the best unmatched before entity for this after entity
            let mut best_unmatched: Option<(&SemanticEntity, f64)> = None;
            for entity in &unmatched_before {
                if entity.signature.is_none() {
                    continue;
                }
                if logical_key(entity) != after_key {
                    continue;
                }
                if entity.signature == after_entity.signature {
                    continue; // Same signature → not a signature change
                }
                let score = overload_similarity(entity, after_entity);
                if score > current_score {
                    match best_unmatched {
                        Some((_, best)) if score <= best => {}
                        _ => best_unmatched = Some((entity, score)),
                    }
                }
            }

            if let Some((better_before, _better_score)) = best_unmatched {
                // Break the Phase 1 match: better_before → after_entity is SignatureChanged
                // The old before_entity becomes unmatched (will be handled later)
                matched_before.remove(before_entity.id.as_str());
                matched_before.insert(&better_before.id);

                changes_to_remove.push(change_idx);

                let mut sig_change = make_change(
                    after_entity,
                    ChangeType::SignatureChanged,
                    Some(better_before),
                    commit_sha,
                    author,
                    &combined_by_id,
                );
                sig_change.structural_change = match (&better_before.structural_hash, &after_entity.structural_hash) {
                    (Some(bsh), Some(ash)) => Some(bsh != ash),
                    _ => Some(true),
                };
                new_changes.push(sig_change);
            }
        }

        // Remove broken Phase 1 changes (iterate in reverse to preserve indices)
        for idx in changes_to_remove.into_iter().rev() {
            changes.remove(idx);
        }
        changes.extend(new_changes);

        // Step 2: Match remaining unmatched entities by logical key + content similarity
        // Re-collect unmatched after Step 1 may have changed matched sets
        let unmatched_before: Vec<&SemanticEntity> = before
            .iter()
            .filter(|e| !matched_before.contains(e.id.as_str()))
            .collect();
        let unmatched_after: Vec<&SemanticEntity> = after
            .iter()
            .filter(|e| !matched_after.contains(e.id.as_str()))
            .collect();

        // Group remaining unmatched entities by logical key
        let mut before_by_logical: HashMap<String, Vec<&SemanticEntity>> = HashMap::new();
        for entity in &unmatched_before {
            if entity.signature.is_some() {
                before_by_logical
                    .entry(logical_key(entity))
                    .or_default()
                    .push(entity);
            }
        }

        for after_entity in &unmatched_after {
            if matched_after.contains(after_entity.id.as_str()) {
                continue;
            }
            if after_entity.signature.is_none() {
                continue;
            }
            let key = logical_key(after_entity);
            let candidates = match before_by_logical.get_mut(&key) {
                Some(c) => c,
                None => continue,
            };

            // Find the best content match among unmatched before entities
            let mut best_idx: Option<usize> = None;
            let mut best_score: f64 = 0.0;
            for (i, before_entity) in candidates.iter().enumerate() {
                if matched_before.contains(before_entity.id.as_str()) {
                    continue;
                }
                let score = overload_similarity(before_entity, after_entity);
                if score > best_score {
                    best_score = score;
                    best_idx = Some(i);
                }
            }

            if let Some(idx) = best_idx {
                let before_entity = candidates.remove(idx);
                matched_before.insert(&before_entity.id);
                matched_after.insert(&after_entity.id);

                let mut change = make_change(
                    after_entity,
                    ChangeType::SignatureChanged,
                    Some(before_entity),
                    commit_sha,
                    author,
                    &combined_by_id,
                );
                change.structural_change = Some(best_score < 0.9);
                changes.push(change);
            }
        }
    }

    // Re-collect unmatched after Phase 1.5
    let unmatched_before: Vec<&SemanticEntity> = before
        .iter()
        .filter(|e| !matched_before.contains(e.id.as_str()))
        .collect();
    let unmatched_after: Vec<&SemanticEntity> = after
        .iter()
        .filter(|e| !matched_after.contains(e.id.as_str()))
        .collect();

    // Phase 2: Content hash match (rename/move detection)
    let mut before_by_hash: HashMap<&str, Vec<&SemanticEntity>> = HashMap::new();
    let mut before_by_structural: HashMap<&str, Vec<&SemanticEntity>> = HashMap::new();
    for entity in &unmatched_before {
        before_by_hash
            .entry(entity.content_hash.as_str())
            .or_default()
            .push(entity);
        if let Some(ref sh) = entity.structural_hash {
            before_by_structural
                .entry(sh.as_str())
                .or_default()
                .push(entity);
        }
    }

    for after_entity in &unmatched_after {
        if matched_after.contains(after_entity.id.as_str()) {
            continue;
        }
        // Try exact content_hash first
        let found = before_by_hash
            .get_mut(after_entity.content_hash.as_str())
            .and_then(|c| c.pop());
        // Fall back to structural_hash (formatting/comment changes don't matter)
        let found = found.or_else(|| {
            after_entity.structural_hash.as_ref().and_then(|sh| {
                before_by_structural.get_mut(sh.as_str()).and_then(|c| {
                    c.iter()
                        .position(|e| !matched_before.contains(e.id.as_str()))
                        .map(|i| c.remove(i))
                })
            })
        });

        if let Some(before_entity) = found {
            matched_before.insert(&before_entity.id);
            matched_after.insert(&after_entity.id);

            // If name, file, and parent are the same, only the parent qualifier in the ID changed
            // (e.g. parent class was renamed). Skip — the entity itself is unchanged.
            // But if parent_id differs, this is an intra-file move (e.g. method moved between classes).
            if before_entity.name == after_entity.name
                && before_entity.file_path == after_entity.file_path
                && before_entity.content_hash == after_entity.content_hash
                && before_entity.parent_id == after_entity.parent_id
            {
                continue;
            }

            changes.push(make_change(after_entity, classify_match(before_entity, after_entity), Some(before_entity), commit_sha, author, &combined_by_id));
        }
    }

    // Phase 3: Same logical signature across a file rename.
    // A file path change changes entity IDs, so renamed files with edited
    // entities need a signature fallback to avoid add/delete pairs.
    for after_entity in &unmatched_after {
        if matched_after.contains(after_entity.id.as_str()) {
            continue;
        }

        let mut best_match: Option<&SemanticEntity> = None;
        let mut best_score = f64::NEG_INFINITY;

        for before_entity in &unmatched_before {
            if matched_before.contains(before_entity.id.as_str()) {
                continue;
            }
            if !same_signature_across_file_rename(before_entity, after_entity, &before_by_id, &after_by_id) {
                continue;
            }

            let score = similarity_fn
                .map(|f| f(before_entity, after_entity))
                .unwrap_or_else(|| default_similarity(before_entity, after_entity));
            if score > best_score {
                best_score = score;
                best_match = Some(before_entity);
            }
        }

        if let Some(before_entity) = best_match {
            matched_before.insert(&before_entity.id);
            matched_after.insert(&after_entity.id);
            changes.push(make_change(after_entity, classify_match(before_entity, after_entity), Some(before_entity), commit_sha, author, &combined_by_id));
        }
    }

    // Phase 4: Fuzzy similarity (>80% threshold)
    // Optimized: pre-compute token sets once per entity, group by type
    let still_unmatched_before: Vec<&SemanticEntity> = unmatched_before
        .iter()
        .filter(|e| !matched_before.contains(e.id.as_str()))
        .copied()
        .collect();
    let still_unmatched_after: Vec<&SemanticEntity> = unmatched_after
        .iter()
        .filter(|e| !matched_after.contains(e.id.as_str()))
        .copied()
        .collect();

    if !still_unmatched_before.is_empty() && !still_unmatched_after.is_empty() {
        const THRESHOLD: f64 = 0.8;
        const SIZE_RATIO_CUTOFF: f64 = 0.5;

        // Pre-compute token sets once per entity (N+M instead of N×M allocations)
        let before_sets: Vec<HashSet<&str>> = still_unmatched_before
            .iter()
            .map(|e| e.content.split_whitespace().collect())
            .collect();
        let after_sets: Vec<HashSet<&str>> = still_unmatched_after
            .iter()
            .map(|e| e.content.split_whitespace().collect())
            .collect();

        // Group before entities by type: O(sum(n_t × m_t)) instead of O(N×M)
        let mut before_by_type: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, e) in still_unmatched_before.iter().enumerate() {
            before_by_type
                .entry(e.entity_type.as_str())
                .or_default()
                .push(i);
        }

        for (ai, after_entity) in still_unmatched_after.iter().enumerate() {
            let candidates = match before_by_type.get(after_entity.entity_type.as_str()) {
                Some(indices) => indices,
                None => continue,
            };

            let a_set = &after_sets[ai];
            let a_len = a_set.len();
            let mut best_idx: Option<usize> = None;
            let mut best_score: f64 = 0.0;

            for &bi in candidates {
                if matched_before.contains(still_unmatched_before[bi].id.as_str()) {
                    continue;
                }

                let b_set = &before_sets[bi];
                let b_len = b_set.len();

                // Size ratio filter using pre-computed set lengths
                let (min_l, max_l) = if a_len < b_len {
                    (a_len, b_len)
                } else {
                    (b_len, a_len)
                };
                if max_l > 0 && (min_l as f64 / max_l as f64) < SIZE_RATIO_CUTOFF {
                    continue;
                }

                // Inline Jaccard on pre-computed sets
                let intersection = a_set.intersection(b_set).count();
                let union = a_len + b_len - intersection;
                let score = if union == 0 {
                    0.0
                } else {
                    intersection as f64 / union as f64
                };

                if score >= THRESHOLD && score > best_score {
                    best_score = score;
                    best_idx = Some(bi);
                }
            }

            if let Some(bi) = best_idx {
                let matched = still_unmatched_before[bi];
                matched_before.insert(&matched.id);
                matched_after.insert(&after_entity.id);

                // If name, file, and parent are the same, only the parent qualifier changed.
                if matched.name == after_entity.name
                    && matched.file_path == after_entity.file_path
                    && matched.content_hash == after_entity.content_hash
                    && matched.parent_id == after_entity.parent_id
                {
                    continue;
                }

                changes.push(make_change(after_entity, classify_match(matched, after_entity), Some(matched), commit_sha, author, &combined_by_id));
            }
        }
    }

    // Phase 5: Intra-file reorder detection
    // For entities that matched by exact ID with identical content (unchanged),
    // check if their relative ordering changed within the file.
    detect_reorders(before, after, &matched_before, &matched_after, &mut changes, commit_sha, author, &combined_by_id);

    // Remaining unmatched before = deleted
    for entity in before.iter().filter(|e| !matched_before.contains(e.id.as_str())) {
        changes.push(make_change(entity, ChangeType::Deleted, Some(entity), commit_sha, author, &combined_by_id));
    }

    // Remaining unmatched after = added
    for entity in after.iter().filter(|e| !matched_after.contains(e.id.as_str())) {
        changes.push(make_change(entity, ChangeType::Added, None, commit_sha, author, &combined_by_id));
    }

    MatchResult { changes }
}

/// Compute similarity score between two overload variants for optimal pairing.
/// Uses a tiered scoring system:
///   1.0  — content_hash exact match (body identical, only signature changed)
///   0.9  — structural_hash match (body structurally identical, cosmetic diff)
///   0.0-0.8 — Jaccard token similarity on content
fn overload_similarity(a: &SemanticEntity, b: &SemanticEntity) -> f64 {
    // Tier 1: exact content hash match
    if a.content_hash == b.content_hash {
        return 1.0;
    }
    // Tier 2: structural hash match (excludes name + cosmetic changes)
    if let (Some(ref sha), Some(ref shb)) = (&a.structural_hash, &b.structural_hash) {
        if sha == shb {
            return 0.9;
        }
    }
    // Tier 3: Jaccard token similarity
    default_similarity(a, b)
}

/// Jaccard token similarity between two strings.
/// Splits both strings on whitespace, computes |intersection| / |union|.
pub fn jaccard_str_similarity(a: &str, b: &str) -> f64 {
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();
    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    intersection as f64 / union as f64
}

/// Default content similarity using Jaccard index on whitespace-split tokens
pub fn default_similarity(a: &SemanticEntity, b: &SemanticEntity) -> f64 {
    let tokens_a: Vec<&str> = a.content.split_whitespace().collect();
    let tokens_b: Vec<&str> = b.content.split_whitespace().collect();

    // Early rejection: if token counts differ too much, Jaccard can't reach 0.8
    let (min_c, max_c) = if tokens_a.len() < tokens_b.len() {
        (tokens_a.len(), tokens_b.len())
    } else {
        (tokens_b.len(), tokens_a.len())
    };
    if max_c > 0 && (min_c as f64 / max_c as f64) < 0.6 {
        return 0.0;
    }

    jaccard_str_similarity(&a.content, &b.content)
}

/// Detect intra-file reordering of unchanged entities.
///
/// Takes entities that matched by exact ID with identical content and checks
/// if their relative ordering changed. Uses longest increasing subsequence
/// (LIS) on the "after" positions to find the minimum set of moved entities.
fn detect_reorders(
    before: &[SemanticEntity],
    after: &[SemanticEntity],
    matched_before: &HashSet<&str>,
    matched_after: &HashSet<&str>,
    changes: &mut Vec<SemanticChange>,
    commit_sha: Option<&str>,
    author: Option<&str>,
    by_id: &HashMap<&str, &SemanticEntity>,
) {
    // Collect unchanged entities: matched by ID with same content_hash
    let before_by_id: HashMap<&str, &SemanticEntity> =
        before.iter().map(|e| (e.id.as_str(), e)).collect();

    // Group by file. For each file, collect unchanged entities in their
    // before-order, then look up their after-positions.
    let mut by_file: HashMap<&str, Vec<(&SemanticEntity, &SemanticEntity)>> = HashMap::new();
    for after_entity in after {
        if !matched_after.contains(after_entity.id.as_str()) {
            continue;
        }
        if let Some(before_entity) = before_by_id.get(after_entity.id.as_str()) {
            if !matched_before.contains(before_entity.id.as_str()) {
                continue;
            }
            // Only consider truly unchanged entities (same content)
            if before_entity.content_hash != after_entity.content_hash {
                continue;
            }
            // Only intra-file
            if before_entity.file_path != after_entity.file_path {
                continue;
            }
            by_file
                .entry(after_entity.file_path.as_str())
                .or_default()
                .push((before_entity, after_entity));
        }
    }

    for (_file, pairs) in &mut by_file {
        if pairs.len() < 2 {
            continue;
        }

        // Sort by before start_line to get the "before" ordering
        pairs.sort_by_key(|(b, _)| b.start_line);

        // Map to after start_lines in before-order
        let after_lines: Vec<usize> = pairs.iter().map(|(_, a)| a.start_line).collect();

        // Find LIS indices (entities that stayed in relative order)
        let lis_set = longest_increasing_subsequence_indices(&after_lines);

        // Entities NOT in LIS were reordered
        for (i, (_before_entity, after_entity)) in pairs.iter().enumerate() {
            if lis_set.contains(&i) {
                continue;
            }
            changes.push(make_change(after_entity, ChangeType::Reordered, None, commit_sha, author, by_id));
        }
    }
}

/// Find indices that form the longest increasing subsequence.
/// Returns a HashSet of indices in the original array that are part of the LIS.
fn longest_increasing_subsequence_indices(seq: &[usize]) -> HashSet<usize> {
    let n = seq.len();
    if n == 0 {
        return HashSet::new();
    }

    // tails[i] = index in seq of the smallest tail element for IS of length i+1
    let mut tails: Vec<usize> = Vec::new();
    // parent[i] = index of previous element in the LIS ending at seq[i]
    let mut parent: Vec<Option<usize>> = vec![None; n];
    // tail_idx[i] = index in seq that tails[i] points to
    let mut tail_idx: Vec<usize> = Vec::new();

    for i in 0..n {
        let pos = tails.partition_point(|&t| t < seq[i]);
        if pos == tails.len() {
            tails.push(seq[i]);
            tail_idx.push(i);
        } else {
            tails[pos] = seq[i];
            tail_idx[pos] = i;
        }
        parent[i] = if pos > 0 { Some(tail_idx[pos - 1]) } else { None };
    }

    // Trace back to find actual LIS indices
    let mut result = HashSet::new();
    let mut idx = *tail_idx.last().unwrap();
    result.insert(idx);
    while let Some(p) = parent[idx] {
        result.insert(p);
        idx = p;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::build_entity_id;
    use crate::utils::hash::content_hash;

    fn make_entity(id: &str, name: &str, content: &str, file_path: &str) -> SemanticEntity {
        SemanticEntity {
            id: id.to_string(),
            file_path: file_path.to_string(),
            entity_type: "function".to_string(),
            name: name.to_string(),
            signature: None,
            parent_id: None,
            content: content.to_string(),
            content_hash: content_hash(content),
            structural_hash: None,
            start_line: 1,
            end_line: 1,
            metadata: None,
        }
    }

    #[test]
    fn test_exact_match_modified() {
        let before = vec![make_entity("a::f::foo", "foo", "old content", "a.ts")];
        let after = vec![make_entity("a::f::foo", "foo", "new content", "a.ts")];
        let result = match_entities(&before, &after, "a.ts", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Modified);
    }

    #[test]
    fn test_exact_match_unchanged() {
        let before = vec![make_entity("a::f::foo", "foo", "same", "a.ts")];
        let after = vec![make_entity("a::f::foo", "foo", "same", "a.ts")];
        let result = match_entities(&before, &after, "a.ts", None, None, None);
        assert_eq!(result.changes.len(), 0);
    }

    #[test]
    fn test_added_deleted() {
        let before = vec![make_entity("a::f::old", "old", "content", "a.ts")];
        let after = vec![make_entity("a::f::new", "new", "different", "a.ts")];
        let result = match_entities(&before, &after, "a.ts", None, None, None);
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.contains(&ChangeType::Deleted));
        assert!(types.contains(&ChangeType::Added));
    }

    #[test]
    fn test_content_hash_rename() {
        let before = vec![make_entity("a::f::old", "old", "same content", "a.ts")];
        let after = vec![make_entity("a::f::new", "new", "same content", "a.ts")];
        let result = match_entities(&before, &after, "a.ts", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Renamed);
    }

    #[test]
    fn test_same_signature_file_rename_with_content_change_is_moved() {
        let before = vec![make_entity(
            "old.ts::function::foo",
            "foo",
            "export function foo() { return alpha + beta + gamma; }",
            "old.ts",
        )];
        let after = vec![make_entity(
            "new.ts::function::foo",
            "foo",
            "export function foo() { return one + two + three; }",
            "new.ts",
        )];

        let result = match_entities(&before, &after, "new.ts", None, None, None);

        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Moved);
        assert_eq!(result.changes[0].old_file_path.as_deref(), Some("old.ts"));
    }

    #[test]
    fn test_parent_child_dedup_class_method() {
        // Class entity contains the method body in its content.
        // parent_id stores the full entity ID of the parent.
        let class_before = SemanticEntity {
            id: "a.ts::class::DataStack".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "class".to_string(),
            name: "DataStack".to_string(),
            signature: None,
            parent_id: None,
            content: "class DataStack { constructor() {} genPg() { old } }".to_string(),
            content_hash: content_hash("class DataStack { constructor() {} genPg() { old } }"),
            structural_hash: None,
            start_line: 1,
            end_line: 10,
            metadata: None,
        };
        let method_before = SemanticEntity {
            id: "a.ts::a.ts::class::DataStack::genPg".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "method".to_string(),
            name: "genPg".to_string(),
            signature: None,
            parent_id: Some("a.ts::class::DataStack".to_string()),
            content: "genPg() { old }".to_string(),
            content_hash: content_hash("genPg() { old }"),
            structural_hash: None,
            start_line: 5,
            end_line: 8,
            metadata: None,
        };

        let class_after = SemanticEntity {
            id: "a.ts::class::DataStack".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "class".to_string(),
            name: "DataStack".to_string(),
            signature: None,
            parent_id: None,
            content: "class DataStack { constructor() {} genPg() { new } }".to_string(),
            content_hash: content_hash("class DataStack { constructor() {} genPg() { new } }"),
            structural_hash: None,
            start_line: 1,
            end_line: 10,
            metadata: None,
        };
        let method_after = SemanticEntity {
            id: "a.ts::a.ts::class::DataStack::genPg".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "method".to_string(),
            name: "genPg".to_string(),
            signature: None,
            parent_id: Some("a.ts::class::DataStack".to_string()),
            content: "genPg() { new }".to_string(),
            content_hash: content_hash("genPg() { new }"),
            structural_hash: None,
            start_line: 5,
            end_line: 8,
            metadata: None,
        };

        let before = vec![class_before, method_before];
        let after = vec![class_after, method_after];
        let result = match_entities(&before, &after, "a.ts", None, None, None);

        // match_entities no longer deduplicates — suppression happens in differ.rs.
        // Both the class and the method are Modified here.
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.iter().all(|t| *t == ChangeType::Modified));
    }

    #[test]
    fn test_parent_not_deduped_when_no_child_changes() {
        // Only the class-level content changes (e.g. a field added), no method changes
        let class_before = SemanticEntity {
            id: "a.ts::class::Foo".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "class".to_string(),
            name: "Foo".to_string(),
            signature: None,
            parent_id: None,
            content: "class Foo { bar() {} }".to_string(),
            content_hash: content_hash("class Foo { bar() {} }"),
            structural_hash: None,
            start_line: 1,
            end_line: 5,
            metadata: None,
        };
        let method_before = SemanticEntity {
            id: "a.ts::a.ts::class::Foo::bar".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "method".to_string(),
            name: "bar".to_string(),
            signature: None,
            parent_id: Some("a.ts::class::Foo".to_string()),
            content: "bar() {}".to_string(),
            content_hash: content_hash("bar() {}"),
            structural_hash: None,
            start_line: 2,
            end_line: 4,
            metadata: None,
        };

        let class_after = SemanticEntity {
            id: "a.ts::class::Foo".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "class".to_string(),
            name: "Foo".to_string(),
            signature: None,
            parent_id: None,
            content: "class Foo { x = 1; bar() {} }".to_string(),
            content_hash: content_hash("class Foo { x = 1; bar() {} }"),
            structural_hash: None,
            start_line: 1,
            end_line: 6,
            metadata: None,
        };
        let method_after = SemanticEntity {
            id: "a.ts::a.ts::class::Foo::bar".to_string(),
            file_path: "a.ts".to_string(),
            entity_type: "method".to_string(),
            name: "bar".to_string(),
            signature: None,
            parent_id: Some("a.ts::class::Foo".to_string()),
            content: "bar() {}".to_string(),
            content_hash: content_hash("bar() {}"),
            structural_hash: None,
            start_line: 3,
            end_line: 5,
            metadata: None,
        };

        let before = vec![class_before, method_before];
        let after = vec![class_after, method_after];
        let result = match_entities(&before, &after, "a.ts", None, None, None);

        // Class changed but method didn't, so class should still appear
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].entity_name, "Foo");
        assert_eq!(result.changes[0].change_type, ChangeType::Modified);
    }

    fn make_entity_with_parent(id: &str, name: &str, content: &str, file_path: &str, parent_id: Option<&str>) -> SemanticEntity {
        SemanticEntity {
            id: id.to_string(),
            file_path: file_path.to_string(),
            entity_type: "method".to_string(),
            name: name.to_string(),
            signature: None,
            parent_id: parent_id.map(String::from),
            content: content.to_string(),
            content_hash: content_hash(content),
            structural_hash: None,
            start_line: 1,
            end_line: 1,
            metadata: None,
        }
    }

    #[test]
    fn test_intra_file_move_between_classes() {
        // Method moves from ClassA to ClassB in the same file
        let before = vec![make_entity_with_parent(
            "a.rs::class::ClassA::foo", "foo", "fn foo() { do_thing() }",
            "a.rs", Some("a.rs::class::ClassA"),
        )];
        let after = vec![make_entity_with_parent(
            "a.rs::class::ClassB::foo", "foo", "fn foo() { do_thing() }",
            "a.rs", Some("a.rs::class::ClassB"),
        )];
        let result = match_entities(&before, &after, "a.rs", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Moved);
        assert_eq!(result.changes[0].old_parent_id, Some("a.rs::class::ClassA".to_string()));
    }

    #[test]
    fn test_same_parent_is_rename_not_move() {
        // Same parent, different name = rename (not move)
        // Content must be identical (same hash) so Phase 2 catches it
        let body = "fn method(&self) { let x = self.compute(); self.validate(x); self.store(x) }";
        let before = vec![make_entity_with_parent(
            "a.rs::class::Foo::old_method", "old_method", body,
            "a.rs", Some("a.rs::class::Foo"),
        )];
        let after = vec![make_entity_with_parent(
            "a.rs::class::Foo::new_method", "new_method", body,
            "a.rs", Some("a.rs::class::Foo"),
        )];
        let result = match_entities(&before, &after, "a.rs", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Renamed);
        assert!(result.changes[0].old_parent_id.is_none());
    }

    fn make_entity_at(id: &str, name: &str, content: &str, file_path: &str, line: usize) -> SemanticEntity {
        SemanticEntity {
            id: id.to_string(),
            file_path: file_path.to_string(),
            entity_type: "function".to_string(),
            name: name.to_string(),
            signature: None,
            parent_id: None,
            content: content.to_string(),
            content_hash: content_hash(content),
            structural_hash: None,
            start_line: line,
            end_line: line + 2,
            metadata: None,
        }
    }

    #[test]
    fn test_reorder_detection() {
        let before = vec![
            make_entity_at("a::f::alpha", "alpha", "fn alpha() {}", "a.rs", 1),
            make_entity_at("a::f::beta", "beta", "fn beta() {}", "a.rs", 5),
            make_entity_at("a::f::gamma", "gamma", "fn gamma() {}", "a.rs", 9),
        ];
        let after = vec![
            make_entity_at("a::f::alpha", "alpha", "fn alpha() {}", "a.rs", 1),
            make_entity_at("a::f::gamma", "gamma", "fn gamma() {}", "a.rs", 5),
            make_entity_at("a::f::beta", "beta", "fn beta() {}", "a.rs", 9),
        ];
        let result = match_entities(&before, &after, "a.rs", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Reordered);
        // Either beta or gamma is marked, LIS picks the minimum
        assert!(result.changes[0].entity_name == "beta" || result.changes[0].entity_name == "gamma");
    }

    #[test]
    fn test_no_reorder_when_order_preserved() {
        let before = vec![
            make_entity_at("a::f::alpha", "alpha", "fn alpha() {}", "a.rs", 1),
            make_entity_at("a::f::beta", "beta", "fn beta() {}", "a.rs", 5),
        ];
        let after = vec![
            make_entity_at("a::f::alpha", "alpha", "fn alpha() {}", "a.rs", 1),
            make_entity_at("a::f::beta", "beta", "fn beta() {}", "a.rs", 10),
        ];
        let result = match_entities(&before, &after, "a.rs", None, None, None);
        // Lines shifted but relative order is same, no reorder
        assert_eq!(result.changes.len(), 0);
    }

    #[test]
    fn test_default_similarity() {
        let a = make_entity("a", "a", "the quick brown fox", "a.ts");
        let b = make_entity("b", "b", "the quick brown dog", "a.ts");
        let score = default_similarity(&a, &b);
        assert!(score > 0.5);
        assert!(score < 1.0);
    }

    #[test]
    fn parent_name_terminates_on_cyclic_parent_id() {
        // Two entities whose parent_id chains form a cycle. parent_name
        // would loop forever without the visited-set guard.
        let a = make_entity_with_parent("A", "A", "", "f", Some("B"));
        let b = make_entity_with_parent("B", "B", "", "f", Some("A"));
        let mut by_id: HashMap<&str, &SemanticEntity> = HashMap::new();
        by_id.insert("A", &a);
        by_id.insert("B", &b);
        // Synthesize a leaf whose parent_id enters the cycle via A.
        let leaf = make_entity_with_parent("L", "L", "", "f", Some("A"));
        let chain = parent_name(&leaf, &by_id);
        // Must terminate. We don't assert exact contents — order/composition
        // depends on which side of the cycle is reached first; the safety
        // property is "this returns at all."
        assert!(chain.is_some());
    }

    // Helper: create an entity with a signature for overload tests
    fn make_sig_entity(
        name: &str,
        signature: Option<&str>,
        content: &str,
        file_path: &str,
        parent_id: Option<&str>,
    ) -> SemanticEntity {
        let entity_type = if parent_id.is_some() { "method" } else { "function" };
        let id = build_entity_id(file_path, entity_type, name, signature, parent_id);
        SemanticEntity {
            id,
            file_path: file_path.to_string(),
            entity_type: entity_type.to_string(),
            name: name.to_string(),
            signature: signature.map(String::from),
            parent_id: parent_id.map(String::from),
            content: content.to_string(),
            content_hash: content_hash(content),
            structural_hash: None,
            start_line: 1,
            end_line: 1,
            metadata: None,
        }
    }

    #[test]
    fn test_overloaded_methods_have_different_ids() {
        // Two overloads of Process should have different entity IDs
        let e1 = make_sig_entity("Process", Some("(int)"), "void Process(int x)", "a.cs", Some("Calc"));
        let e2 = make_sig_entity("Process", Some("(string)"), "void Process(string s)", "a.cs", Some("Calc"));
        assert_ne!(e1.id, e2.id, "Overloaded methods must have distinct IDs");
    }

    #[test]
    fn test_overloaded_methods_both_tracked_as_separate() {
        // Before: two overloads Process(int) and Process(string)
        // After: Process(int) modified, Process(string) unchanged
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { return 1; }", "a.cs", Some("Calc")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { return 2; }", "a.cs", Some("Calc")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { return 3; }", "a.cs", Some("Calc")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { return 2; }", "a.cs", Some("Calc")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Only Process(int) was modified; Process(string) unchanged
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Modified);
        // The modified entity should contain "(int)" in its ID
        assert!(result.changes[0].entity_id.contains("(int)"));
    }

    #[test]
    fn test_overloaded_method_replaced_with_different_sig() {
        // Before: Process(int) only
        // After: Process(string) only (old overload removed, new one added)
        // Phase 1.5 detects this as a signature change (same logical method name)
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x)", "a.cs", Some("Calc")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(string)"), "void Process(string s)", "a.cs", Some("Calc")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Phase 1.5 matches by logical key (same name, same parent) → SignatureChanged
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::SignatureChanged);
        assert_eq!(result.changes[0].old_signature.as_deref(), Some("(int)"));
    }

    #[test]
    fn test_different_name_methods_deleted_and_added() {
        // When method names are truly different (not just signature change),
        // we should get proper delete + add
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x)", "a.cs", Some("Calc")),
        ];
        let after = vec![
            make_sig_entity("Handle", Some("(int)"), "void Handle(int x)", "a.cs", Some("Calc")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Different names, different content → deleted + added
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.contains(&ChangeType::Deleted));
        assert!(types.contains(&ChangeType::Added));
    }

    #[test]
    fn test_signature_change_detected() {
        // Process(int) → Process(string): same method name, different parameter signature
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { doWork(); }", "a.cs", Some("Calc")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { doWork(); }", "a.cs", Some("Calc")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::SignatureChanged);
        assert_eq!(result.changes[0].old_signature.as_deref(), Some("(int)"));
    }

    #[test]
    fn test_signature_change_with_body_change() {
        // Process(int) → Process(string) with body also changed
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { old(); }", "a.cs", Some("Calc")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { new(); }", "a.cs", Some("Calc")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::SignatureChanged);
        assert_eq!(result.changes[0].old_signature.as_deref(), Some("(int)"));
    }

    #[test]
    fn test_overload_one_modified_one_signature_changed() {
        // Two overloads: one modified (body change), one signature change
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { old(); }", "a.cs", Some("Calc")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { same(); }", "a.cs", Some("Calc")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { new(); }", "a.cs", Some("Calc")),
            make_sig_entity("Process", Some("(bool)"), "void Process(bool b) { same(); }", "a.cs", Some("Calc")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Process(int) → Modified (body changed, signature same)
        // Process(string) → SignatureChanged (→ Process(bool), body same structure)
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.contains(&ChangeType::Modified));
        assert!(types.contains(&ChangeType::SignatureChanged));
    }

    #[test]
    fn test_no_signature_entities_unaffected() {
        // Entities without signatures should still work as before
        let before = vec![make_entity("a::f::foo", "foo", "old content", "a.ts")];
        let after = vec![make_entity("a::f::foo", "foo", "new content", "a.ts")];
        let result = match_entities(&before, &after, "a.ts", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Modified);
    }

    #[test]
    fn test_overload_three_variants() {
        // Three overloads: one unchanged, one modified, one added
        let before = vec![
            make_sig_entity("Do", Some("(int)"), "void Do(int x) { a(); }", "a.cs", Some("C")),
            make_sig_entity("Do", Some("(string)"), "void Do(string s) { b(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Do", Some("(int)"), "void Do(int x) { a(); }", "a.cs", Some("C")),  // unchanged
            make_sig_entity("Do", Some("(string)"), "void Do(string s) { b2(); }", "a.cs", Some("C")),  // modified
            make_sig_entity("Do", Some("(bool)"), "void Do(bool b) { c(); }", "a.cs", Some("C")),  // added
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.contains(&ChangeType::Modified));
        assert!(types.contains(&ChangeType::Added));
        // Make sure the modified one is Do(string), not Do(int)
        let modified = result.changes.iter().find(|c| c.change_type == ChangeType::Modified).unwrap();
        assert!(modified.entity_id.contains("(string)"));
    }

    // ─── Overload count change scenarios ────────────────────────────────

    #[test]
    fn test_overload_count_increases_new_is_added() {
        // Before: Process(int) — single overload
        // After: Process(int), Process(string) — overload added
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { f(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { f(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { g(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Process(int) matched by Phase 1 (unchanged). Process(string) → Added.
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Added);
        assert!(result.changes[0].entity_id.contains("(string)"));
    }

    #[test]
    fn test_overload_count_decreases_old_is_deleted() {
        // Before: Process(int), Process(string) — two overloads
        // After: Process(int) — one overload deleted
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { f(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { g(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { f(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Process(int) matched by Phase 1 (unchanged). Process(string) → Deleted.
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::Deleted);
        assert!(result.changes[0].entity_id.contains("(string)"));
    }

    #[test]
    fn test_overload_same_count_signature_swap_with_content_matching() {
        // Same overload count (2), both signatures changed.
        // Content similarity determines the pairing:
        //   Process(int){bodyA} → Process(double){bodyA}  (same body → SignatureChanged)
        //   Process(string){bodyB} → Process(bool){bodyB}  (same body → SignatureChanged)
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { bodyA(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { bodyB(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(double)"), "void Process(double x) { bodyA(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(bool)"), "void Process(bool b) { bodyB(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Both should be SignatureChanged, paired by content similarity
        assert_eq!(result.changes.len(), 2);
        assert!(result.changes.iter().all(|c| c.change_type == ChangeType::SignatureChanged));

        // Verify correct pairing: (int)→(double) and (string)→(bool)
        let int_to_double = result.changes.iter().find(|c| c.old_signature.as_deref() == Some("(int)"));
        let string_to_bool = result.changes.iter().find(|c| c.old_signature.as_deref() == Some("(string)"));
        assert!(int_to_double.is_some(), "should find (int)→(double) pairing");
        assert!(string_to_bool.is_some(), "should find (string)→(bool) pairing");

        let int_change = int_to_double.unwrap();
        assert!(int_change.entity_id.contains("(double)"), "(int) should pair with (double) via bodyA");
    }

    #[test]
    fn test_overload_mixed_add_and_signature_change() {
        // Before: Process(int){bodyA}, Process(string){bodyB}  (2 overloads)
        // After: Process(bool){bodyA}, Process(string){bodyB}, Process(double){bodyC}  (3 overloads)
        //
        // Process(string) matches by Phase 1 (unchanged).
        // Process(int){bodyA} → Process(bool){bodyA} : SignatureChanged (content matches)
        // Process(double){bodyC} : Added (no match)
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { bodyA(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { bodyB(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(bool)"), "void Process(bool b) { bodyA(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { bodyB(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(double)"), "void Process(double x) { bodyC(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.contains(&ChangeType::SignatureChanged));
        assert!(types.contains(&ChangeType::Added));

        // Verify: (int)→(bool) by content, (double) is new
        let sig_changed = result.changes.iter().find(|c| c.change_type == ChangeType::SignatureChanged).unwrap();
        assert_eq!(sig_changed.old_signature.as_deref(), Some("(int)"));
        assert!(sig_changed.entity_id.contains("(bool)"));
    }

    #[test]
    fn test_overload_mixed_delete_and_signature_change() {
        // Before: Process(int){bodyA}, Process(string){bodyB}, Process(double){bodyC}  (3)
        // After: Process(bool){bodyA}, Process(string){bodyB}  (2)
        //
        // Process(string) matches by Phase 1 (unchanged).
        // Process(int){bodyA} → Process(bool){bodyA} : SignatureChanged (content matches)
        // Process(double){bodyC} : Deleted (no match)
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { bodyA(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { bodyB(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(double)"), "void Process(double x) { bodyC(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(bool)"), "void Process(bool b) { bodyA(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { bodyB(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.contains(&ChangeType::SignatureChanged));
        assert!(types.contains(&ChangeType::Deleted));

        // Verify: (int)→(bool) by content, (double) deleted
        let sig_changed = result.changes.iter().find(|c| c.change_type == ChangeType::SignatureChanged).unwrap();
        assert_eq!(sig_changed.old_signature.as_deref(), Some("(int)"));
        let deleted = result.changes.iter().find(|c| c.change_type == ChangeType::Deleted).unwrap();
        assert!(deleted.entity_id.contains("(double)"));
    }

    #[test]
    fn test_single_overload_signature_change_is_always_signature_changed() {
        // User's rule #1: if there's only one method with that name, signature change
        // is always SignatureChanged (never delete+add)
        let before = vec![
            make_sig_entity("Handle", Some("(int)"), "void Handle(int x) { unique_body(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Handle", Some("(string,int)"), "void Handle(string s, int n) { completely_different(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        assert_eq!(result.changes.len(), 1);
        assert_eq!(result.changes[0].change_type, ChangeType::SignatureChanged);
        assert_eq!(result.changes[0].old_signature.as_deref(), Some("(int)"));
    }

    #[test]
    fn test_phase1_break_when_signature_swap_with_bodies_swapped() {
        // The critical scenario from user's feedback:
        // Before: Process(bool,bool){simple body}, Process(bool,bool,bool){complex body}
        // After:  Process(bool,bool){complex body}, Process(bool,bool,float){simple body}
        //
        // Phase 1 matches (bool,bool)→(bool,bool) as Modified.
        // Phase 1.5 should break this: the complex body after-entity is actually
        // a better match for the (bool,bool,bool) before-entity.
        //
        // Expected result: two SignatureChanged
        let before = vec![
            make_sig_entity("Process", Some("(bool,bool)"),
                "void Process(bool a, bool b) { return simple(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(bool,bool,bool)"),
                "void Process(bool a, bool b, bool c) { int g = computeGroupId(); return complex(g); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(bool,bool)"),
                "void Process(bool a, bool b) { int g = computeGroupId(); return complex(g); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(bool,bool,float)"),
                "void Process(bool a, bool b, float x) { return simple(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);

        // Should get 2 SignatureChanged, NOT Modified+Deleted+Added
        assert_eq!(result.changes.len(), 2);
        let types: Vec<ChangeType> = result.changes.iter().map(|c| c.change_type).collect();
        assert!(types.iter().all(|t| *t == ChangeType::SignatureChanged),
            "expected all SignatureChanged, got {:?}", types);

        // Verify correct pairing by checking old_signature
        let sig_changes: Vec<_> = result.changes.iter()
            .filter(|c| c.change_type == ChangeType::SignatureChanged)
            .collect();
        let old_sigs: Vec<&str> = sig_changes.iter()
            .filter_map(|c| c.old_signature.as_deref())
            .collect();
        assert!(old_sigs.contains(&"(bool,bool)"), "should have (bool,bool) as old sig");
        assert!(old_sigs.contains(&"(bool,bool,bool)"), "should have (bool,bool,bool) as old sig");
    }

    #[test]
    fn test_phase1_not_broken_when_content_match_is_worse() {
        // Don't break Phase 1 match if the unmatched entity is NOT a better content match
        // Before: Process(int){bodyA}, Process(string){bodyB}
        // After:  Process(int){bodyA_modified}, Process(string){bodyB_modified}
        let before = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { bodyA(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { bodyB(); }", "a.cs", Some("C")),
        ];
        let after = vec![
            make_sig_entity("Process", Some("(int)"), "void Process(int x) { bodyA_changed(); }", "a.cs", Some("C")),
            make_sig_entity("Process", Some("(string)"), "void Process(string s) { bodyB_changed(); }", "a.cs", Some("C")),
        ];
        let result = match_entities(&before, &after, "a.cs", None, None, None);
        // Both should be Modified (Phase 1 matches, Phase 1.5 doesn't break them)
        assert_eq!(result.changes.len(), 2);
        assert!(result.changes.iter().all(|c| c.change_type == ChangeType::Modified));
    }
}
