---
name: sem
description: Semantic code analysis — use sem when you need to understand WHAT changed, not WHERE. Activates for code review, refactoring, debugging, and codebase exploration.
---

You have access to `sem`, a semantic code analysis CLI that understands code at the entity level (functions, classes, methods) rather than lines.

## When to Use sem

**Use sem when you need to reason about code changes, dependencies, or entity history.** Do NOT use sem for general file searching, git operations, or text editing.

Activate sem in these situations:

| Situation | Command | Why |
|-----------|---------|-----|
| "What changed?" | `sem diff` | Git diff shows lines. Sem shows which functions/classes were added, modified, deleted, moved, or renamed. |
| "Is this change safe?" | `sem impact <entity>` | Shows all dependents that would break if this entity changes. |
| "How did this function evolve?" | `sem log <entity>` | Traces entity across commits, handles renames and file moves automatically. |
| "What's in this file?" | `sem entities <path>` | Lists all functions/classes with their signatures and line ranges. |
| "I need context on this entity" | `sem context <entity>` | Returns token-budgeted surrounding context for LLM consumption. |
| "Did a refactor break callers?" | `sem verify --diff` | Catches arity mismatches between call sites and signature changes. |
| "Who wrote this function?" | `sem blame <file>` | Per-entity blame instead of per-line. |

## Procedures

### Code Review / PR Analysis

```
1. sem diff --format markdown       → get structured list of changed entities
2. For each modified/deleted entity:
   sem impact <entity> --dependents → check who depends on it
3. sem verify --diff                → catch broken call sites from signature changes
```

### Refactoring Preparation

```
1. sem impact <entity>             → map all deps + dependents + affected tests
2. sem context <entity> --budget 16000 → get full context including dependencies
3. (refactor code)
4. sem diff -v                     → review the semantic diff
5. sem verify --diff               → confirm no arity mismatches
```

### Debugging / Understanding Unfamiliar Code

```
1. sem entities <file>             → discover what entities exist and their signatures
3. sem log -v <entity>             → trace how this entity changed over time
```

### Overloaded Methods

All methods display their parameter signature — `()` for no params, `(int,string)` for params. When a method has multiple overloads, you MUST specify the signature:
```bash
# Step 1: List entities to discover signatures
sem entities path/to/file.cs
#   method Process() (L10:20)
#   method Process(int) (L25:35)

# Step 2: Use the signature in quotes (including empty parens for paramless)
sem log "Process(int)"
sem log "Process()"
```

## Quick Reference

| Flag | Purpose |
|------|---------|
| `--file <path>` | Filter to one file (diff, log, impact, context) |
| `--format markdown` | markdown output |
| `-v` | Show inline content diffs |
| `--file-exts .py .rs` | Filter by language |
| `--limit N` | Max entity changes to show (log, default 10) |

## Tips

- Always use `--format markdown` for commands
- `sem diff --format markdown --file <path>` is more efficient than parsing full diff output when you only care about one file
