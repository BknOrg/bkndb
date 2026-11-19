---
name: coderecall
description: >-
  Code graph, semantic recall, blast-radius analysis, and feature boundary extraction
  for multi-language repositories (Go, C, C++, Rust, Java, JavaScript, TypeScript).
  Use before reading or grepping raw files when exploring symbols, tracing callers/callees,
  assessing change impact, reviewing diffs, or extracting feature boundaries for refactoring.
---

# CodeRecall Agent Guide

Use **CodeRecall** first to understand code structure, call relationships, and change impact instead of scanning or grepping raw files blindly.

## Tool Priority

1. **Prefer MCP tools (`coderecall_*`)** when connected to the CodeRecall MCP server.
2. **Fallback to CLI** (`coderecall <command> --json`) if MCP tools are unavailable in the current session.

---

## Recommended Workflows

### 1. Orient Yourself in a Repository
- **`coderecall_status`**: Check indexed files, node/edge counts, languages, and unresolved ref counts.
- **`coderecall_digest`**: Get a compact architectural summary of key symbols and modules.
- **`coderecall_report`**: Identify architectural hubs, feature communities, bridges, and dependency cycles.

### 2. Search & Trace Symbols
- **`coderecall_search`** (`query`, optional `kind`, `limit`): Find symbols via BM25 fulltext search.
- **`coderecall_explore`** (`target`, optional `min_conf`): Inspect 1-hop callers, callees, `IMPLEMENTS`, and file relations.
- **`coderecall_explain`** (`target`): Get symbol signature, source snippet, and risk flags.
- **`coderecall_path`** (`from`, `to`, optional `depth`): Find the shortest directed dependency/call path between two symbols.

### 3. Assess Blast Radius Before Editing
- **`coderecall_impact`** (`target`, optional `depth`): Reverse BFS blast radius showing everything that depends on the target.
- **`coderecall_affected`** (`target`, optional `depth`): Find test functions/files impacted by changing a symbol or file.
- **`coderecall_review`** (optional `base`): Analyze `git diff` blast radius, impacted tests, and risk score.

### 4. Refactor or Separate a Feature Safely
- **`coderecall_boundary`** (`target`, optional `use_lsp: true`): Partition a symbol, file, or directory into:
  - **Internal Core**: Safe to move/rewrite internally.
  - **Inbound Seam**: External callers/contracts that must be preserved.
  - **Outbound Dependencies**: External dependencies to mock or invert.
  - **Shared Kernel**: Symbols shared with other features (do not delete).
- Set `use_lsp: true` when high precision is needed on ambiguous dynamic/method calls and a language server (`gopls`, `rust-analyzer`, `clangd`, `jdtls`, `typescript-language-server`) is installed.

### 5. Audit Code Health
- **`coderecall_cycles`**: Detect circular symbol or file dependencies (Tarjan SCC).
- **`coderecall_dead_code`** (optional `include_exported`): Find unreachable symbols with zero callers.

---

## CLI Fallback Quick Reference

```bash
coderecall sync [--lsp]
coderecall status --json
coderecall search "<query>" --json
coderecall explore <symbol> --json
coderecall impact <symbol> --depth 3 --json
coderecall boundary <symbol|file|dir> [--lsp] --json
coderecall review [--base main] --json
```
