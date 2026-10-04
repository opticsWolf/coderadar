"""Text renderings of `coderadar.ops` results.

MCP returns these strings as tool results and the CLI prints them, so an
operation reads the same on every surface (`--format json` on the CLI and
the Python API return the data itself). Renderers take data and never
touch the graph; staleness banners and "no index" guidance are the
caller's, since they depend on how the surface found its project.
"""

from __future__ import annotations

import re

from .ops import friendly_entity_id


def not_found(entity_id: str, candidates: list[dict], hint: str = "codegraph_search") -> str:
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
        "Escape hatches: `codegraph_search_similar` (semantic, embedding-based) "
        "or `codegraph_explore` with explicit `symbols` for known names."
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
            return f"No handler found for route `{name}`. Try codegraph_search."
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
                f"Try codegraph_search for a broader search.")
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
