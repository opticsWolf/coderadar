"""codegraph_search — Symbol discovery tool (§26.2)

Hybrid keyword + vector search for finding symbols when you don't know
the exact name. Returns ranked results with snippets.
"""

from __future__ import annotations

from typing import Any, Dict, List


async def hybrid_search(graph: Any, args: Dict[str, Any]) -> str:
    """Search for symbols by keyword or description.

    Args:
        graph: CodeGraph instance.
        args: {query: str, kind?: str, top_k?: int}

    Returns:
        Ranked search results.
    """
    query = args.get("query", "")
    if not query.strip():
        return "Please provide a `query` to search for."

    if graph is None:
        return "No index available. Run `coderadar init` first."

    kind_filter = args.get("kind")
    top_k = min(args.get("top_k") or 10, 20)

    results = _search(graph, query, top_k, kind_filter)

    if not results:
        return (
            f"No results found for '{query}'"
            + (f" (kind: {kind_filter})" if kind_filter else "")
            + ". Try broader terms or a different kind filter."
        )

    return _render_results(results, query)


def _search(
    graph: Any, query: str, top_k: int, kind_filter: str | None,
) -> List[Dict[str, Any]]:
    """Execute the search via Rust backend."""
    try:
        from coderadar._core import search_entities
        results = search_entities(query, top_k)
        if kind_filter:
            results = [r for r in results if r.get("kind") == kind_filter]
        return results[:top_k]
    except ImportError:
        return _fallback_search(graph, query, top_k, kind_filter)


def _fallback_search(
    graph: Any, query: str, top_k: int, kind_filter: str | None,
) -> List[Dict[str, Any]]:
    """Fallback: simple name-based search."""
    results: List[Dict[str, Any]] = []
    query_lower = query.lower()

    for kind in ["function", "class", "method", "module"]:
        if kind_filter and kind != kind_filter:
            continue
        try:
            from coderadar._core import scan_entities
            for entity in scan_entities(kind):
                name = entity.get("name", "").lower()
                if query_lower in name:
                    results.append(entity)
                    if len(results) >= top_k:
                        return results
        except ImportError:
            pass

    return results


def _render_results(results: List[Dict[str, Any]], query: str) -> str:
    """Format search results."""
    lines = [
        f"## Search: `{query}`",
        f"Found {len(results)} result(s)",
        "",
    ]

    for i, entity in enumerate(results, 1):
        name = entity.get("name", "?")
        kind = entity.get("kind", "?")
        file_path = entity.get("file_path", "?")
        entity_id = entity.get("id", "?")
        start_line = entity.get("start_line")

        lines.append(f"### {i}. `{name}` ({kind})")
        lines.append(f"- **ID:** `{entity_id}`")
        lines.append(f"- **File:** `{file_path}`")
        if start_line:
            lines.append(f"- **Line:** {start_line}")

        docstring = entity.get("docstring")
        if docstring:
            snippet = docstring[:200] + ("..." if len(docstring) > 200 else "")
            lines.append(f"- **Docstring:** {snippet}")

        signature = entity.get("signature")
        if signature:
            lines.append(f"- **Signature:** `{signature}`")

        lines.append("")

    return "\n".join(lines)
