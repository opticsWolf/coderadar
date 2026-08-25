# CodeRadar — Bug & Quirks Report

**Environment:** CodeRadar MCP server (`coderadar mcp serve`), pi coding-agent harness, Windows
**Project exercised:** `D:/User/Documents/Python/stitch_crawler` (Python + Rust, 34 files)
**Date:** 2026-08-24 · **Session:** audit → smell triage → live mutation refactor

---

## Summary

| # | Issue | Severity | Type |
|---|-------|----------|------|
| 1 | `replace_body` writes body dedented by one level → corrupts file | **Critical (bug)** | Mutation |
| 2 | Mutation failure returns three contradictory signals | High (bug) | API/Mutation |
| 3 | In-process graph ignores config changes until project re-switch | High (bug) | Indexing |
| 4 | Docstring is part of "body" — silently deleted on replace | Medium (quirk) | Mutation |
| 5 | Entity ID format undocumented; wrong format silently "not found" | Medium (quirk) | API |
| 6 | Pest query grammar contradicts documented examples | Medium (bug) | Query |
| 7 | Error message recommends workaround the tool forbids | Low (quirk) | UX |
| 8 | Background indexing makes early calls return empty, not "warming up" | Low (quirk) | Indexing |
| 9 | Duplicate smell findings for the same entity | Low (bug) | Smells |
| 10 | `mcpScript` tool-name prefixing trips up scripted batches | Info (quirk) | API |

---

## 1. 🔴 `replace_body` corrupts indentation on write

**What happened:** Applied a `replace_body` mutation to
`meta_extractor.py::merge_into_document_metadata`. Every line of the replacement body
was written **dedented by one level (4 spaces stripped)** — except the first line of the
body, which kept its indent:

```python
) -> Dict[str, Any]:
    """                       # ← first line kept 4 spaces
Merge HTML metadata ...       # ← all following lines written at column 0
...
return merged                 # ← SyntaxError: 'return' outside function
```

Result: the target module became unparseable (`SyntaxError: 'return' outside function`).
Had this been applied without immediate verification, it would have broken the package.

**Repro:** any multi-line body passed to `coderadar_replace_body` with `dry_run=False`.
The dry-run diff renders correctly (indented), so the defect is specifically in the
*apply* path's rendering — dry run and apply disagree.

**Impact:** Critical — silent source corruption; only caught because the caller ran
`ast.parse` / tests immediately after.

**Workaround:** verify with `ast.parse()` immediately after every apply; repair via
external edit + `codegraph_update_file` to resync the graph (this worked cleanly).

**Fix suggestion:** run the same syntax validation on the *written file*, not just the
body string, and roll the file back if it fails.

## 2. 🟠 Mutation failure reports three contradictory things at once

Response to a rejected mutation:

```
## Mutation Applied
- **Status:** RejectedPolicy
- **Syntax errors:** 1
Graph has been updated — subsequent queries will reflect the change.
```

- Header says "**Mutation Applied**", status says **RejectedPolicy**.
- File was **not** written (verified on disk), yet "Graph has been updated" implies it was.
- "Syntax errors: 1" is unexplained — the replacement body was valid Python both times;
  unclear whether it refers to the body, the file, or the aborted result.

A client that checks only the header (or trusts "graph updated") will draw the wrong
conclusion on every axis.

**Fix suggestion:** one unambiguous status enum; state explicitly whether the file was
written and whether the graph changed; drop or explain the syntax-error field.

## 3. 🟠 Config changes are invisible to the running server until you switch away and back

**Sequence observed:**

1. Added `target/**` to `exclude` in `.coderadar.toml`.
2. `codegraph_reindex` (MCP) → still indexed 34 files (build artifacts included).
3. CLI `coderadar analyze` with the same config → correctly 13 files.
4. CLI `coderadar rebuild` → store on disk correct (13 files).
5. `codegraph_reindex` again → **still 34 files**.
6. `codegraph_set_project` away → back → now respects the config.

The MCP server caches `[project].exclude` (and, per #5 below, `[mutation]`) from
startup/project-switch and does **not** re-read them on `reindex`, even though the
underlying store was rebuilt correctly by the CLI. This cost several confusing
round-trips where two views of the same project disagreed.

**Fix suggestion:** `reindex` should re-read project config, or return the effective
config (roots/excludes) so callers can detect staleness.

## 4. 🟡 `replace_body` treats the docstring as body — silently deleted

The dry-run diff showed the function's entire NumPy-style docstring removed because the
tool defines "body" as *everything under the signature line*. Nothing warns about this;
a user replacing logic casually strips documentation.

**Workaround:** always re-include the docstring text in `new_body`.
**Fix suggestion:** preserve leading docstrings automatically, or flag their removal in
the plan/diff summary.

## 5. 🟡 Entity ID format is undocumented and exact-match-or-fail

- `codegraph_explore(symbols=[...])` returns IDs like
  `.\py_web_researcher\meta_extractor.py::merge_into_document_metadata`
- `codegraph_node` / `codegraph_affected` reject the "obvious" spellings:
  - `py_web_researcher/meta_extractor.py::merge_into_document_metadata` → *not found*
  - forward slashes fail; the leading `.\` and backslash separators are mandatory.

There is no normalization and no fuzzy fallback ("did you mean…"), so callers must copy
IDs verbatim from a previous tool output.

**Fix suggestion:** normalize separators/case for lookups, or accept the plain
`file::symbol` form.

## 6. 🟡 Pest query grammar rejects its own documented examples

Tool description advertises: *"Supports: … 'entities where kind = function'"*. Actual:

```
query: "entities where kind = function"
→ Parse error: positives: [entity] … location Pos(0)
```

The parser expects singular `entity` at position 0 while the docs say `entities`.
Additionally, `functions where name = 'step'` returned no results for a symbol that
`get_smells` and `codegraph_search` both knew about (same session, post-index) — so
either the equality operator behaves differently than `contains` in surprising ways, or
the query ran against a different snapshot than the smell detector (cf. #3/#8).

## 7. ⚪ Error message recommends the thing the tool then refuses

When calling any tool with a `project_path` other than the served root:

> Start a second server for that project with `coderadar mcp serve --path <root>`, or
> drop the `project_path` argument…

…but the intended flow is actually `codegraph_set_project`, which is the tool designed
for exactly this. The guidance sends users toward spawning extra servers instead of the
built-in switch.

## 8. ⚪ Background indexing looks identical to "no results"

After `set_project`, `codegraph_search` / `codegraph_query` returned empty result sets
(no items, no error). There is no "index warming up" signal — empty and not-yet-indexed
are indistinguishable, which invites wrong conclusions during scripted fan-outs.
`set_project` does say "tool calls will wait for it," but evidently read paths don't
block until indexing completes.

## 9. ⚪ Duplicate smell findings

Pre-cleanup, `get_smells` listed the same entity multiple times (e.g. `step`
long-method/complexity twice, `get` long-method three times) — consistent with stale +
fresh versions of the same entity coexisting in the store after incremental updates.
Findings should be keyed by current entity version only.

## 10. ⚪ Tool-name prefixing inside `mcpScript`

Inside batched scripts, tools must be called by their fully-prefixed names
(`tools.coderadar_codegraph_query`, not `tools.codegraph_query`) — unprefixed calls fail
with `tool_not_found` listing the prefixed alternative. Documented nowhere obvious;
cost a debugging round-trip.

---

## What worked well

- **Dry-run diffs** for mutations are clear and genuinely caught problems before apply (#4).
- **`affected` blast radius** correctly enumerated 1 production caller + 8 tests before refactoring.
- **`update_file`** resync-after-external-edit worked perfectly.
- **CLI/MCP split** (`analyze`, `rebuild` from shell) provided a trustworthy second opinion.
- **Smell detection quality** itself was good — findings mapped cleanly to real issues,
  and the generated-code false positives were our own fault (no `target/` exclusion out
  of the box — consider default-excluding build dirs).

## Recommended actions (for CodeRadar maintainers)

1. **Block the ship-stopper:** validate the *written file* parses after mutation; auto-rollback on failure (#1).
2. Make mutation status single-sourced and truthful (#2).
3. Re-read project config on `reindex`, or expose effective config (#3).
4. Preserve docstrings on body replace (#4) and normalize entity-ID lookup (#5).
5. Align Pest grammar/docs (#6), fix misleading error guidance (#7), surface indexing progress (#8), dedupe smells (#9).
