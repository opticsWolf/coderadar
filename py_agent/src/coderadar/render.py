"""Text renderings of `coderadar.ops` results.

MCP returns these strings as tool results and the CLI prints them, so an
operation reads the same on every surface (`--format json` on the CLI and
the Python API return the data itself). Renderers take data and never
touch the graph; staleness banners and "no index" guidance are the
caller's, since they depend on how the surface found its project.
"""

from __future__ import annotations

import re
from typing import Any

from .ops import display_file, friendly_entity_id


def not_found(entity_id: str, candidates: list[dict], hint: str = "coderadar_search") -> str:
    """A miss with candidates instead of a bare `not found`."""
    lines = [f"Entity `{entity_id}` not found."]
    if candidates:
        lines.append("Did you mean:")
        for hit in candidates[:3]:
            lines.append(f"- `{hit.get('name')}` — `{hit.get('id')}`")
    else:
        lines.append(f"Try {hint} to locate it.")
    return "\n".join(lines)


def search_miss(query: str, kind: str | None) -> str:
    """A miss that says what was actually tried.

    "Try broader terms" made an empty result look like the index was missing
    the thing, when the truth is "no indexed name, signature or docstring
    contains any of these tokens" — so the agent kept retrying variations
    instead of switching tools.
    """
    tokens = [t for t in re.split(r"\s+", query.strip()) if t]
    out = [
        "No results found for '" + query + "'"
        + (f" (kind: {kind})" if kind else "")
        + "."
    ]
    if len(tokens) >= 2:
        shown = ", ".join(f"`{t}`" for t in tokens[:8])
        out.append(
            f"Tokens are matched independently (OR) — none of {shown} occurs "
            "in any indexed entity's name, signature or docstring."
        )
        out.append("Try a single token by itself, one that you expect as an identifier.")
    else:
        out.append(
            "The token was matched against entity names, function signatures "
            "and docstrings and hit nothing."
        )
    out.append(
        "Escape hatches: `coderadar_search_similar` (semantic, embedding-based) "
        "or `coderadar_explore` with explicit `symbols` for known names."
    )
    return "\n".join(out)


def search(query: str, kind: str | None, results: list[dict]) -> str:
    if not results:
        return search_miss(query, kind)
    lines = [f"## Search: `{query}`", f"Found {len(results)} result(s)", ""]
    for i, entity in enumerate(results, 1):
        lines.append(f"### {i}. `{entity.get('name', '?')}` ({entity.get('kind', '?')})")
        lines.append(f"- **ID:** `{friendly_entity_id(entity.get('id', '?'))}`")
        lines.append(f"- **File:** `{entity.get('file_path', '?')}`")
        sl = entity.get("start_line")
        if sl:
            lines.append(f"- **Line:** {sl}")
        doc = entity.get("docstring")
        if doc:
            lines.append(f"- **Docstring:** {doc[:200]}{'...' if len(doc) > 200 else ''}")
        sig = entity.get("signature")
        if sig:
            lines.append(f"- **Signature:** `{sig}`")
        lines.append("")
    return "\n".join(lines)


def node(entity: dict) -> str:
    lines = [
        f"## {entity.get('name', '?')}",
        "",
        f"- **ID:** `{friendly_entity_id(entity.get('id', '?'))}`",
        f"- **Kind:** {entity.get('kind', '?')}",
        f"- **File:** `{entity.get('file_path', '?')}`",
    ]
    start = entity.get("start_line")
    end = entity.get("end_line")
    if start and end:
        lines.append(f"- **Lines:** {start}–{end} ({end - start + 1} lines)")
    docstring = entity.get("docstring")
    if docstring:
        lines.append(f"\n```\n{docstring}\n```")
    signature = entity.get("signature")
    if signature:
        lines.append(f"\n**Signature:** `{signature}`")
    decorators = entity.get("decorators", [])
    if decorators:
        lines.append(f"\n**Decorators:** {', '.join(f'`{d}`' for d in decorators)}")
    grammar_kind = entity.get("grammar_kind")
    if grammar_kind:
        lines.append(f"\n**Grammar kind:** `{grammar_kind}`")
    for key, title in (("callers", "Callers"), ("callees", "Callees")):
        items = entity.get(key) or []
        if items:
            lines.append(f"\n## {title} ({len(items)})")
            for c in items[:15]:
                lines.append(f"- `{c.get('name', c.get('id', '?'))}` ({c.get('kind', '?')})")
    return "\n".join(lines)


def affected(entity_id: str, result: dict) -> str:
    depths: dict[int, list[dict]] = result["depths"]
    central = set(result.get("central_ids") or [])
    total = sum(len(v) for v in depths.values())
    lines = [
        f"## Affected by `{result['entity'].get('name', '?')}`",
        "",
        f"Transitive impact for `{entity_id}` (max depth: {result['max_depth']})",
        f"**Total dependents:** {total}",
        "",
    ]
    if total == 0:
        lines.append("No dependents found. Nothing calls this entity.")
        return "\n".join(lines)
    for depth, entities in depths.items():
        indent = "  " * depth
        lines.append(f"**Depth {depth}** ({len(entities)}):")
        for e in entities[:20]:
            ei = friendly_entity_id(e.get("id", "?"))
            mark = " ⭐" if ei in central else ""
            lines.append(f"{indent}- `{e.get('name', '?')}` ({e.get('kind', '?')}) — `{ei}`{mark}")
        if len(entities) > 20:
            lines.append(f"{indent}  ... and {len(entities) - 20} more")
        lines.append("")
    return "\n".join(lines)


def module_children(result: dict) -> str:
    cats = ("classes", "functions", "imports", "constants")
    total = sum(len(result.get(k, [])) for k in cats)
    lines = [f"## Module: `{result['module_id']}`", f"{total} children", ""]
    for category in cats:
        items = result.get(category, [])
        if not items:
            continue
        lines.append(f"### {category.title()} ({len(items)})")
        for item in items:
            name = item.get("name", item.get("id", "?"))
            line_no = item.get("line", item.get("start_line", ""))
            extra = f" (line {line_no})" if line_no else ""
            lines.append(f"- `{name}`{extra} — `{item.get('id', '')}`")
        lines.append("")
    return "\n".join(lines)


def resolve(result: dict) -> str:
    name, results = result["name"], result["results"]
    if result["mode"] == "route":
        if not results:
            return f"No handler found for route `{name}`. Try coderadar_search."
        lines = [f"## Route Resolution: `{name}`", f"Found {len(results)} handler(s)", ""]
        for i, r in enumerate(results, 1):
            lines.append(f"### {i}. `{r.get('name', '?')}` ({r.get('kind', '?')}) — "
                         f"confidence {r.get('confidence', 0):.2f}")
            lines.append(f"- **ID:** `{friendly_entity_id(r.get('id', '?'))}`")
            lines.append(f"- **File:** `{r.get('file_path', '?')}`")
            route = r.get("route")
            if route:
                lines.append(f"- **Route:** `{route.get('name', '?')}`")
            lines.append("")
        return "\n".join(lines)
    if not results:
        return (f"No framework resolver claimed `{name}`. "
                f"Try coderadar_search for a broader search.")
    lines = [f"## Reference Resolution: `{name}`", f"Found {len(results)} result(s)", ""]
    for i, r in enumerate(results, 1):
        lines.append(f"### {i}. `{r.get('name', '?')}` ({r.get('kind', '?')}) — "
                     f"{r.get('resolved_by', 'unknown')} (confidence {r.get('confidence', 0):.2f})")
        lines.append(f"- **ID:** `{friendly_entity_id(r.get('id', '?'))}`")
        lines.append(f"- **File:** `{r.get('file_path', '?')}`")
        sig = r.get("signature")
        if sig:
            lines.append(f"- **Signature:** `{sig}`")
        lines.append("")
    return "\n".join(lines)


def dead_code(findings: list[dict], min_confidence: float) -> str:
    if not findings:
        return (
            "No dead code found at or above confidence "
            f"{min_confidence:.2f}. This is a clean result for the current "
            "index — not an indexing failure."
        )
    lines = [
        f"## Dead Code — {len(findings)} finding(s) at confidence >= {min_confidence:.2f}",
        "",
        "Ranked most-safely-deletable first. Verify each with `affected` before removal.",
        "",
    ]
    for f in findings:
        loc = f.get("file", "?")
        line = f.get("line", 0)
        loc_str = f" — `{loc}:{line}`" if line else (f" — `{loc}`" if loc != "?" else "")
        lines.append(
            f"- **{f.get('entity_name', '?')}** (`{f['entity_id']}`) — {f['kind']}, "
            f"{f['tier']} ({f['score']:.2f}), ~{f['removable_lines']} lines{loc_str}"
        )
        why = list(f.get("evidence") or [])
        if f.get("nearest_root_distance") is not None:
            why.append(f"{f['nearest_root_distance']} hop(s) from a live root")
        if why:
            lines.append(f"  - why: {'; '.join(why)}")
    return "\n".join(lines)


def get_smells(findings: list[dict], entity_id: str | None, rule_id: str | None) -> str:
    if not findings:
        scope = []
        if entity_id:
            scope.append(f"entity `{entity_id}`")
        if rule_id:
            scope.append(f"rule={rule_id}")
        suffix = f" for {' and '.join(scope)}" if scope else ""
        return f"## Code smells\n\nNo findings{suffix}."
    lines = ["## Code smells", f"Found {len(findings)} finding(s)", ""]
    for f in findings:
        name = f.get("entity_name") or f.get("entity_id", "?")
        lines.append(
            f"- **[{f.get('severity', '?')}]** `{f.get('rule_id', '?')}` — "
            f"{name}: {f.get('message', '')}"
        )
        signals = f.get("signals") or {}
        if signals:
            lines.append(f"  - signals: {', '.join(f'{k}={v:g}' for k, v in signals.items())}")
    return "\n".join(lines)


def find_clones(groups: list[dict], min_lines: int, min_similarity: float) -> str:
    if not groups:
        return (
            f"No clone groups found at >= {min_similarity:.2f} similarity and "
            f">= {min_lines} lines. Clean result — not an indexing failure."
        )
    lines = [f"## Clone Groups — {len(groups)} group(s)", ""]
    for gi, g in enumerate(groups, 1):
        lines.append(
            f"### Group {gi} — {g['clone_type']}, similarity {g['similarity']:.2f}, "
            f"{g['confidence_tier']}"
        )
        for inst in g["instances"]:
            # Lines first: a reviewer reads line numbers, not byte offsets.
            # The span stays for anything that slices.
            lines.append(
                f"- `{inst['entity_id']}` ({inst['file']} @ lines "
                f"{inst['start_line']}-{inst['end_line']}, bytes "
                f"{inst['span_start']}..{inst['span_end']})"
            )
        lines.append("")
    lines.append(
        "Consider extracting shared logic; verify each pair with `explore` before refactoring."
    )
    if any(g.get("reason") == "literal-table" for g in groups):
        lines.append(
            "`literal-table` groups are key/value data that happens to share a "
            "shape — a shared data source is usually the fix, not shared logic."
        )
    return "\n".join(lines)


SCAFFOLD_ORDER = ("placeholder-body", "secret", "comment-marker", "temp-file")


def find_scaffolding(findings: list[dict], include_secrets: bool) -> str:
    if not findings:
        return (
            "No AI scaffolding signals found"
            + (" (secrets included)" if include_secrets else "")
            + ". Clean result — not an indexing failure."
        )
    by_kind: dict[str, list] = {}
    for f in findings:
        by_kind.setdefault(f["kind"], []).append(f)
    # The trailing scan-stats row is a footer, not a finding.
    stats_rows = by_kind.pop("scan-stats", [])
    n_findings = sum(len(by_kind.get(k, [])) for k in SCAFFOLD_ORDER)
    lines = [f"## Scaffolding Signals — {n_findings} finding(s)", ""]
    for kind in SCAFFOLD_ORDER:
        items = by_kind.get(kind, [])
        if not items:
            continue
        lines.append(f"### {kind.replace('-', ' ').title()} ({len(items)})")
        for it in items[:25]:
            loc = f"{it['file']}:{it['line']}" if it["line"] else str(it["file"])
            lines.append(f"- `{loc}` — {it['label']}: {it['snippet']}")
        if len(items) > 25:
            lines.append(f"- … and {len(items) - 25} more")
        lines.append("")
    for s in stats_rows:
        lines.append(f"_{s['label']}_")
    return "\n".join(lines)


# ── Staleness ─────────────────────────────────────────────────────────────
# Adapted from CodeGraph's formatStaleBanner / formatDegradedBanner
# (MIT License, https://github.com/colbymchenry/codegraph)

def stale_banner(stale_files: list[dict], referenced_paths: list[str]) -> str:
    """A warning naming the referenced files edited since the last sync.

    Only files in `referenced_paths` (those the response actually uses) are
    named; other stale files are noise to a reader about to act on these.
    """
    if not stale_files:
        return ""
    referenced_set = set(referenced_paths)
    relevant = [s for s in stale_files if s["path"] in referenced_set]
    if not relevant:
        return ""
    lines = [
        (
            "⚠️ Some files referenced below were edited since the last index sync — "
            "their codegraph entries may be stale:"
        ),
    ]
    for s in relevant:
        lines.append(f"  - {s['path']}")
    lines.append(
        "For accurate content of those specific files, Read them directly. "
        "Every file NOT listed above is fresh — still trust codegraph."
    )
    lines.append("")
    return "\n".join(lines)


# ── Explore ───────────────────────────────────────────────────────────────
# Output budget adapted from CodeGraph's getExploreOutputBudget /
# allocateExploreBudget (MIT License, https://github.com/colbymchenry/codegraph)

MAX_OUTPUT_CHARS = 18_000
"""Hard cap on total explore output (characters)."""

MAX_CHARS_PER_FILE = 4_500
"""Maximum source characters served per file."""

POINTER_HEADER = "**Not shown above — explore these names for their source**"
"""Header for files trimmed by the output budget."""


def explore_usage() -> str:
    return (
        "Please provide symbol names or a question to explore. "
        'For example: coderadar_explore(query="User.save authenticate")'
    )


def explore_miss(names: list[str]) -> str:
    name_list = ", ".join(f"`{n}`" for n in names)
    return (
        f"Couldn't find {name_list} in the index. Each name was matched "
        "exactly, then as search tokens, against names, signatures and "
        "docstrings. Try a single well-known symbol, `coderadar_search` "
        "with one token, or `coderadar_search_similar` for semantic search."
    )


def explore(result: dict) -> str:
    """Source grouped by file, then the call relationships, within the
    output budget."""
    lines: list[str] = []
    stale = result.get("stale_files") or []
    banner = stale_banner(stale, [s["path"] for s in stale])
    if banner:
        lines.append(banner)

    for f in result["files"]:
        entities = f["entities"]
        names_str = ", ".join(
            f"{e.get('name', '?')}({e.get('kind', '?')})" for e in entities[:10]
        )
        lines.append(f"**{f['file_path']}** — {names_str}")
        lines.append("")
        for entity in entities:
            if entity.get("source"):
                lines.append(entity["source"])
                lines.append("")

    rel_lines = []
    for r in result["relationships"]:
        if r["kind"] == "caller":
            rel_lines.append(f"- `{r['other_name']}` ←──[caller] `{r['entity_name']}`")
        else:
            rel_lines.append(f"- `{r['entity_name']}` ──→[callee] `{r['other_name']}`")
    if rel_lines:
        lines.append("## Relationships")
        lines.extend(rel_lines)

    return apply_output_budget(lines)


def apply_output_budget(
    lines: list[str],
    max_chars: int = MAX_OUTPUT_CHARS,
    max_per_file: int = MAX_CHARS_PER_FILE,
) -> str:
    """Trim full output to fit within a character budget.

    Strategy: walk through file sections (delimited by ``**file_path**`` headers),
    applying per-file caps first, then a global cap. Files below the cap get
    full source; at-cap files get their source truncated at cluster boundaries.
    Files that don't fit at all are converted to pointer lines.

    Always preserves file headers and the Relationships section.
    """
    text = "\n".join(lines)
    if len(text) <= max_chars:
        return text

    # Identify file sections and the relationships section
    file_sections: list[list[str]] = []
    current_section: list[str] = []
    relationships_lines: list[str] = []
    in_relationships = False

    for line in lines:
        if line.startswith("## Relationships"):
            in_relationships = True
            if current_section:
                file_sections.append(current_section)
                current_section = []
        if in_relationships:
            relationships_lines.append(line)
            continue
        # File header detection: **path** — ...
        if line.startswith("**") and "** —" in line:
            if current_section:
                file_sections.append(current_section)
            current_section = [line]
        elif current_section:
            current_section.append(line)
        else:
            # Preamble lines (before first file header)
            current_section.append(line)

    if current_section:
        file_sections.append(current_section)

    # Separate preamble from file sections
    preamble_lines: list[str] = []
    file_sections_filtered: list[list[str]] = []
    for sec in file_sections:
        if sec and sec[0].startswith("**") and "** —" in sec[0]:
            file_sections_filtered.append(sec)
        else:
            preamble_lines = sec

    # Build output: preamble + truncated file sections + relationships
    output_lines = list(preamble_lines)
    remaining = max_chars - len("\n".join(output_lines))
    if relationships_lines:
        remaining -= len("\n".join(relationships_lines)) + 2  # 2 for separators

    if remaining <= 0:
        # Bare minimum: just relationships
        output_lines = [POINTER_HEADER, ""]
        output_lines.extend(relationships_lines)
        return "\n".join(output_lines)

    pointer_files: list[str] = []

    for sec in file_sections_filtered:
        sec_text = "\n".join(sec)
        if len(sec_text) <= max_per_file:
            # Small enough — include whole (but check global budget)
            if len(sec_text) <= remaining:
                output_lines.extend(sec)
                remaining -= len(sec_text)
            else:
                # Budget exhausted — pointer only
                pointer_files.append(extract_path_from_header(sec[0]))
        else:
            # Per-file cap: trim to max_per_file, preserving the header
            header = sec[0]
            body_lines = sec[1:]
            trimmed_body = trim_to_char_budget(body_lines, max_per_file - len(header) - 1)
            trimmed_sec = [header] + trimmed_body
            sec_text = "\n".join(trimmed_sec)
            if len(sec_text) <= remaining:
                output_lines.extend(trimmed_sec)
                remaining -= len(sec_text)
            else:
                pointer_files.append(extract_path_from_header(header))

    # Pointer list for files that didn't fit
    if pointer_files:
        output_lines.append("")
        output_lines.append(POINTER_HEADER)
        for pf in pointer_files:
            output_lines.append(f"- {pf}")
        output_lines.append("")

    # Relationships
    if relationships_lines:
        output_lines.append("")
        output_lines.extend(relationships_lines)

    return "\n".join(output_lines)


def extract_path_from_header(header: str) -> str:
    """Extract file path from a ``**path** — symbols`` header."""
    return header.removeprefix("**").split("**")[0].strip()


def trim_to_char_budget(body_lines: list[str], max_chars: int) -> list[str]:
    """Trim body source lines to fit within max_chars, at line boundaries."""
    if max_chars <= 0:
        return []
    result: list[str] = []
    used = 0
    for line in body_lines:
        # +1 for newline separator
        cost = len(line) + 1
        if used + cost > max_chars:
            break
        result.append(line)
        used += cost
    if len(result) < len(body_lines):
        result.append("...")
    return result


# ── Traverse / query ──────────────────────────────────────────────────────

def traverse(result: dict) -> str:
    entity = result["entity"]
    title = f"## Traverse from `{entity.get('name', result['entity_id'])}`"
    rows = result["results"]
    depth = result["max_depth"]
    if not rows:
        return (f"{title}\n\nNo neighbors found "
                f"(direction={result['direction']}, max_depth={depth})")

    by_depth: dict[int, list[dict]] = {}
    for r in rows:
        by_depth.setdefault(r.get("depth", 1), []).append(r)

    lines = [
        title,
        (
            f"Direction: {result['direction']}, max depth: {depth}, "
            f"edge kinds: {result['edge_kinds'] or 'all'}"
        ),
        f"Found {len(rows)} reachable entities",
        "",
    ]
    if result.get("unresolved", 0) > 0:
        lines.append(
            f"⚠️ Traversal incomplete: {result['unresolved']} outgoing target(s) "
            f"could not be resolved and were excluded from the walk."
        )
    for d in sorted(by_depth):
        items = by_depth[d]
        lines.append(f"### Depth {d} ({len(items)})")
        for item in items[:15]:
            name = item.get("name", item.get("id", item.get("entity_id", "?")))
            ek = item.get("kind", item.get("edge_type", "?"))
            eid = item.get("id", item.get("entity_id", ""))
            fp = display_file(item)
            fp_str = f" — `{fp}`" if fp and fp != "?" else ""
            id_str = f" — `{eid}`" if eid and eid != name else ""
            lines.append(f"- `{name}` ({ek}){fp_str}{id_str}")
        if len(items) > 15:
            lines.append(f"  ... and {len(items) - 15} more")
        lines.append("")
    return "\n".join(lines)


def query_usage() -> str:
    return ("Please provide a query. Examples:\n"
            "  - classes where inherits_from contains 'BaseModel'\n"
            "  - methods where is_async == true\n"
            "  - functions where name starts_with 'test_'\n"
            "  - imports where import_kind == 'from'\n"
            "Entities: modules, classes, functions, methods, constants, entities, "
            "imports, calls, fields. Full field reference: docs/query-language.md")


def query(query: str, rows: list[dict], limit: int | None = 30) -> str:
    """Numbered rows; `limit` None prints all of them."""
    if not rows:
        return f"Query `{query}` returned no results."
    lines = [f"## Query: `{query}`", f"Found {len(rows)} result(s)", ""]
    shown = rows if limit is None else rows[:limit]
    for i, row in enumerate(shown, 1):
        name = row.get("name", row.get("id", "?"))
        kind = row.get("kind", row.get("entity_type", "?"))
        fp = display_file(row)
        rid = row.get("id", row.get("entity_id", ""))
        sl = row.get("start_line", row.get("line", ""))
        lines.append(f"{i}. `{name}` ({kind}) — `{fp}`")
        if rid:
            lines.append(f"   ID: `{rid}`")
        if sl:
            lines.append(f"   Line: {sl}")
        sig = row.get("signature")
        if sig:
            lines.append(f"   Signature: `{sig}`")
    if len(rows) > len(shown):
        lines.append(f"... and {len(rows) - len(shown)} more")
    return "\n".join(lines)


# ── Embeddings ────────────────────────────────────────────────────────────

def search_similar(query: str, results: list[dict]) -> str:
    if not results:
        return f"No semantically similar results found for '{query}'."
    lines = [f"## Semantic Search: `{query}`", f"Found {len(results)} result(s)", ""]
    for i, r in enumerate(results, 1):
        lines.append(f"{i}. `{r.get('name', '?')}` ({r.get('kind', '?')}) — "
                     f"similarity {r.get('similarity', 0.0):.3f}")
        lines.append(f"   File: `{display_file(r)}`")
        doc = r.get("docstring")
        if doc:
            lines.append(f"   {doc[:120]}{'...' if len(doc) > 120 else ''}")
        lines.append("")
    return "\n".join(lines)


FASTEMBED_MISSING = (
    "Semantic search requires `fastembed` to be installed. "
    "Run: pip install fastembed\n"
    "Then run compute_embeddings() to index all entities."
)

NO_EMBEDDINGS = (
    "No embeddings found and auto-computation failed. "
    "Run coderadar_compute_embeddings first, or "
    "coderadar_reindex with_embeddings=True."
)


def compute_embeddings(metrics: dict) -> str:
    return (
        f"## Embeddings Complete\n\n"
        f"- **Generated:** {metrics.get('generated', 0)}\n"
        f"- **Cached (unchanged):** {metrics.get('cached', 0)}\n"
        f"- **Total entities:** {metrics.get('total', 0)}\n"
        f"- **Errors:** {metrics.get('errors', 0)}\n\n"
        f"Semantic search (coderadar_search_similar) is now available."
    )


# ── Temporal ──────────────────────────────────────────────────────────────

def as_of(result: dict) -> str:
    timestamp = result["timestamp"]
    if not result["names"]:
        # Nothing is loaded at this point; as_of resolves per symbol.
        return "\n".join([
            f"## Snapshot at `{timestamp}`",
            "",
            (
                "Pass `symbols` to look entities up as they were at this "
                "timestamp — for example "
                f'coderadar_as_of(timestamp="{timestamp}", symbols=["User"]).'
            ),
            "",
            (
                "Only symbol lookup is reconstructed from the ledger. "
                "`coderadar_query` and `coderadar_search` always run against the "
                "current index."
            ),
        ])
    lines = [f"## Snapshot at `{timestamp}`", ""]
    for name in result["names"]:
        entity = result["entities"].get(name)
        if entity:
            lines.append(f"**{entity.get('name', name)}** ({entity.get('kind', '?')})")
            lines.append(f"- File: `{entity.get('file_path', '?')}`")
            sig = entity.get("signature")
            if sig:
                lines.append(f"- Signature: `{sig}`")
        else:
            lines.append(f"`{name}` — not found at {timestamp}")
        lines.append("")
    return "\n".join(lines)


# ── Edits ─────────────────────────────────────────────────────────────────

def edit(outcome: dict, apply_hint: str = "call again with `dry_run=False`") -> str:
    """A dry-run plan (ending with how to apply it) or an applied result."""
    note = f"Note: {outcome['note']}\n\n" if outcome.get("note") else ""
    plan = outcome["plan"]
    if outcome["result"] is None:
        return note + mutation_plan(plan) + f"\n**To apply:** {apply_hint}."
    return note + mutation_applied(outcome["result"], plan.unverified_sites)


def mutation_error(raw: str) -> str:
    """Translate raw engine errors into LLM-actionable prose (F10 fix)."""
    if "StaleIndex" in raw or "stale" in raw.lower():
        return (
            "## Mutation Rejected — Stale Index\n\n"
            "The file changed on disk after it was indexed, so the planned "
            "span no longer lines up. Nothing was written.\n\n"
            "**Next step:** run `coderadar_update_file` on the file (or "
            "re-analyze), then retry the mutation.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    if "RejectedPolicy" in raw or "policy" in raw.lower():
        return (
            "## Mutation Rejected — Policy\n\n"
            "The target path is outside the `[mutation] allow` list. "
            "Nothing was written.\n\n"
            "**Next step:** pick a target under an allowed root, or ask the "
            "user to extend the allow list.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    if "SpanOutOfBounds" in raw or "out of bounds" in raw.lower():
        return (
            "## Mutation Failed — Span Mismatch\n\n"
            "The computed edit span fell outside the file — likely a stale "
            "concept or an off-by-one in the planner. Nothing was written "
            "(rollback confirmed).\n\n"
            "**Next step:** update the file in the graph and retry; if it "
            "persists, report it with the entity id.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    if "ParseError" in raw or "syntax" in raw.lower():
        return (
            "## Mutation Failed — Syntax\n\n"
            "The edited file did not re-parse, so the change was rolled "
            "back. Nothing was written.\n\n"
            f"<details>Raw error: `{raw[:300]}`</details>"
        )
    return f"Mutation failed: {raw}"


def mutation_plan(plan: Any) -> str:
    """A MutationPlan (dry run): id, affected files, diff, unverified sites."""
    lines = [f"## Mutation Plan: `{plan.tool}` (DRY RUN)", ""]
    lines.append(f"- **Plan ID:** `{plan.id}`")
    lines.append(f"- **Affected files:** {len(plan.affected_files)}")

    if plan.diff_preview:
        lines.append("")
        lines.append("### Diff Preview")
        lines.append("```diff")
        lines.extend(plan.diff_preview.split("\n")[:60])
        if len(plan.diff_preview.split("\n")) > 60:
            lines.append("...")
        lines.append("```")

    if plan.unverified_sites:
        lines.append("")
        lines.append(
            f"⚠️ **WARNING: {len(plan.unverified_sites)} call site(s) could not be "
            f"verified/rewritten. Manual review required.**"
        )
        for site in plan.unverified_sites[:10]:
            if isinstance(site, dict):
                where = f"{site.get('file', '?')}:{site.get('line', 0)}"
                lines.append(
                    f"- `{where}` — {site.get('reason', '')}"
                    + (f" (`{site['snippet']}`)" if site.get("snippet") else "")
                )
            else:
                lines.append(f"- `{site}`")

    if plan.warnings:
        lines.append("")
        lines.append("### Warnings")
        for w in plan.warnings:
            lines.append(f"- ⚠ {w}")

    return "\n".join(lines)


def mutation_applied(result: Any, unverified_sites: list | None = None) -> str:
    """A MutationResult — truthfully, whatever the outcome.

    BUGS_QUIRKS #2: this used to print "## Mutation Applied" and "Graph has
    been updated" even for rejections, while a separate status line said
    RejectedPolicy and the file was untouched. The header, the state claims,
    and the status now derive from one source: result.status.
    """
    status = str(getattr(result, "status", ""))
    files_written = list(getattr(result, "files_written", []) or [])
    applied = status == "Applied"

    header = {
        "Applied": "## Mutation Applied",
        "RolledBack": "## Mutation Rolled Back",
        "RejectedStale": "## Mutation Rejected — Stale",
        "RejectedPolicy": "## Mutation Rejected — Policy",
    }.get(status, f"## Mutation Result — {status or 'Unknown'}")

    lines = [header, ""]
    lines.append(f"- **Status:** {status}")
    lines.append(f"- **File written:** {'yes' if files_written else 'no'}")
    lines.append(
        f"- **Graph updated:** {'yes' if applied else 'no — nothing changed'}"
    )
    if files_written:
        lines.append(f"- **Files written:** {len(files_written)}")
        for f in files_written:
            lines.append(f"  - `{f}`")
    if result.syntax_errors:
        lines.append(f"- **Syntax errors:** {len(result.syntax_errors)}")
        for e in result.syntax_errors[:5]:
            if isinstance(e, dict):
                where = f"{e.get('file', '?')}:{e.get('line', 0)}:{e.get('column', 0)}"
                lines.append(f"  - `{where}` — {e.get('message', '')}")
            else:
                lines.append(f"  - {e}")
    if getattr(result, "backup_path", None):
        lines.append(f"- **Backup:** `{result.backup_path}`")
    if not applied:
        lines.append("")
        if status == "RejectedStale":
            lines.append(
                "The file changed since planning — call the plan tool again to "
                "re-read the current content, then apply the fresh plan."
            )
        else:
            lines.append("No changes were kept. Address the reason above and re-plan.")
    if unverified_sites:
        lines.append("")
        lines.append(
            f"⚠️ **WARNING: {len(unverified_sites)} call site(s) could not be "
            f"verified/rewritten. Manual review required.**"
        )
        for site in unverified_sites[:10]:
            lines.append(f"- `{site}`")
    return "\n".join(lines)


# ── Index lifecycle ───────────────────────────────────────────────────────

def reindex(result: dict) -> str:
    stats = result["stats"]
    lines = [
        "## Reindex Complete",
        "",
        f"- **Files:** {stats.get('file_count', 0)}",
        f"- **Modules:** {stats.get('modules', 0)}",
        f"- **Classes:** {stats.get('classes', 0)}",
        f"- **Functions:** {stats.get('functions', 0)}",
        f"- **Call edges:** {stats.get('call_edges', 0)}",
    ]
    if "embeddings" in result:
        lines.append("")
        lines.append(f"- **Embeddings generated:** {result['embeddings'].get('generated', 0)}")
        lines.append(f"- **Embeddings cached:** {result['embeddings'].get('cached', 0)}")
    elif "embeddings_error" in result:
        lines.append("")
        lines.append(f"- **Embeddings:** failed — {result['embeddings_error']}")
    return "\n".join(lines)


def update_file(file_path: str, report: Any, from_content: bool) -> str:
    if getattr(report, "removed", False):
        return (
            f"## File Removed\n\n"
            f"- **File:** `{file_path}`\n"
            f"- Not on disk any more: {report.entities_removed} entit"
            f"{'y' if report.entities_removed == 1 else 'ies'} dropped from the graph.\n"
        )
    if not report.fully_applied:
        # tree-sitter recovers rather than failing, so the graph did take
        # entities from the file — just not reliably the ones inside the
        # region it had to recover from.
        return (
            f"## Update Incomplete\n\n"
            f"- **File:** `{file_path}`\n"
            f"- **Parse quality:** {report.parse_quality}\n"
            f"- **Parse errors:** {report.parse_errors}\n"
            f"\nThe file was indexed from a recovered parse — entities in "
            f"the broken region may be missing or wrong. Fix the syntax "
            f"and update again.\n"
        )
    return (
        f"## File Updated\n\n"
        f"- **File:** `{file_path}`\n"
        f"- Graph refreshed from {'provided content' if from_content else 'disk'}.\n"
    )


def status(result: dict) -> str:
    lines = [f"## Project: `{result['root']}`", ""]
    config = result["config"]
    lines.append(f"- **Config:** `{config}`" if config else "- **Config:** none")
    store = result["store"]
    if store:
        fresh = result.get("store_fresh")
        state = "" if fresh is None else (" (fresh)" if fresh else
                                          " (older than some source files)")
        lines.append(f"- **Store:** `{store}`{state}")
    else:
        lines.append("- **Store:** none — run `coderadar init` to create one")
    stats = result.get("stats")
    if not result["loaded"] or stats is None:
        lines.append("- **Index:** not loaded")
        return "\n".join(lines)
    age = result.get("age_seconds")
    when = "" if age is None else (f", synced {age / 60:.0f} min ago" if age >= 60
                                   else ", synced just now")
    lines.append(f"- **Index:** loaded{when}")
    lines.append("")
    for key in ("file_count", "modules", "classes", "functions", "imports",
                "constants", "call_edges"):
        if key in stats:
            lines.append(f"- {key.replace('_', ' ')}: {stats[key]}")
    return "\n".join(lines)
