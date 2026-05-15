# sem (Rust)

Semantic version control CLI. Entity-level diffs, blame, dependency graphs, and impact analysis on top of Git.

Git shows you *line 43 changed*. Sem shows you *function validateToken was modified in src/auth.ts*.

## Why

Git's line-based model doesn't match how developers think. You don't care that lines 12-18 changed. You care that `validateToken` was modified and `legacyAuth` was deleted. This matters even more when agents are making changes, because agents need to reason about *what* changed, not *where* in the file.

## Commands

```bash
# Entity-level diff
sem diff
sem diff --file src/auth.ts              # filter to a specific file
sem diff HEAD~3..HEAD                     # compare refs
sem diff --staged                         # staged changes only

# Entity-level blame (who last touched each function/class)
sem blame src/auth.ts

# List entities in a file or directory
sem entities src/auth.ts

# Cross-file dependency graph
sem graph

# Impact analysis (if this entity changes, what else is affected?)
sem impact validateToken

# Entity history — trace how a function evolved across commits
sem log validateToken
sem log "Process(int,string)"             # overload-aware: specify signature
sem log validateToken --file src/auth.ts  # disambiguate when entity exists in multiple files
sem log validateToken -v                  # show inline content diffs

# Context window for an entity (token-budgeted, for AI agents)
sem context validateToken

# Verify function call arity across the codebase
sem verify

# Filter to specific languages in a multi-language repo
sem graph --file-exts .py
sem diff --file-exts .py .rs
sem impact validateToken --file-exts .py
```

## Overload Support

All methods display their parameter signature (e.g. `()` for no params). When methods have multiple overloads, specify the signature to disambiguate:

```bash
# List entities — every method shows its signature
sem entities src/auth.ts
#   method validateToken() (L10:25)
#   method validateToken(string) (L30:45)

# Log a specific overload by its signature
sem log "validateToken(string)"

# Paramless overload — use empty parens
sem log "validateToken()"

# If no signature is specified and overloads exist, sem will list them
# and ask you to pick one
sem log validateToken
#   error: Entity 'validateToken' has 2 overloads:
#     method validateToken() (L10:25)
#     method validateToken(string) (L30:45)
#   Specify the signature to disambiguate: sem log "validateToken()"
```

`sem log` tracks renames, signature changes, and cross-file moves automatically.

## Languages

23 tree-sitter parser plugins: TypeScript, JavaScript, Python, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift, Elixir, Bash, HCL/Terraform, Kotlin, Fortran, Perl, Dart, OCaml, plus Vue, Svelte, ERB. Shebang detection for extensionless files.

Falls back to chunk-based diffing for unsupported file types.

## Architecture

Cargo workspace with two crates:

```
sem-core/    # Library: entity extraction, structural hashing, semantic diff,
             # dependency graph, impact analysis, git bridge
sem-cli/     # Binary: diff, blame, graph, impact, log, entities, context, verify commands
```

### sem-core

The library that weave, agenthub, effect-system, agent-lint, unified-build, and agent-bench all depend on.

- **Parser registry** with 23 language plugins via tree-sitter + shebang detection
- **Structural hashing** (AST-normalized, ignores whitespace/comments)
- **Semantic diff** with 3-phase entity matching (exact ID, content hash, fuzzy similarity)
- **Overload-aware entity tracking** with signature matching and disambiguation
- **Cosmetic vs structural** change detection
- **Entity dependency graph** (cross-file, call/reference edges)
- **Impact analysis** (transitive BFS through dependency graph)
- **Entity history tracking** across renames, signature changes, and file moves
- **Git bridge** for reading file contents at any ref

## Build

```bash
cargo build --release
# Binary at target/release/sem
```

## Tests

```bash
cargo test
# 283 tests
```

## License

MIT
