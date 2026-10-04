"""One implementation per operation, shared by every surface.

The Python API (`CodeGraph` methods), the MCP tools and the CLI commands
are adapters over the functions here. Each function returns plain data
(dicts and lists) and raises an :class:`OpError` subclass for anything the
caller should be told rather than shown a traceback; `coderadar.render`
turns the data into the text MCP and the CLI print. Operation names follow
the MCP tool names with the `codegraph_` / `coderadar_` prefix dropped.

Everything reads the process-wide graph the native core holds (the one
the last `analyze` / `load` built). Callers bind it to a project: the API
handle through `_bound_to_loaded_project`, the MCP server through its
served root, the CLI through the directory it runs in.
"""

from __future__ import annotations

import os
from collections import deque
from typing import Any

__all__ = [
    "SEARCH_KINDS",
    "EngineError",
    "InvalidRequest",
    "NoExtension",
    "NoIndex",
    "NotFound",
    "OpError",
    "affected",
    "canonical_entity_id",
    "dead_code",
    "display_file",
    "find_clones",
    "find_entity",
    "find_scaffolding",
    "friendly_entity_id",
    "get_smells",
    "module_children",
    "node",
    "resolve",
    "search",
    "suggest_entities",
]


# ── Errors ────────────────────────────────────────────────────────────────

class OpError(Exception):
    """An answer for the caller, not a crash: each surface words it."""


class NoExtension(OpError):
    """The native `coderadar._core` extension is not importable."""


class NoIndex(OpError):
    """No graph is loaded, or the loaded one indexed nothing."""


class InvalidRequest(OpError):
    """The arguments cannot be answered (empty query, unknown kind, …)."""


class NotFound(OpError):
    """An entity or module id matched nothing.

    `candidates` holds near-miss entities (``{name, id}`` dicts) so the
    caller can offer "did you mean".
    """

    def __init__(self, entity_id: str, candidates: list[dict] | None = None,
                 detail: str = ""):
        super().__init__(detail or f"Entity `{entity_id}` not found.")
        self.entity_id = entity_id
        self.candidates = candidates or []
        self.detail = detail


class EngineError(OpError):
    """The native engine failed (or panicked) while answering."""


def _require_index(*kinds: str) -> None:
    """Raise unless the loaded graph has at least one entity of `kinds`.

    `kinds` defaults to modules: any indexed file. Analyses that only make
    sense over code ask for ``functions`` (or ``functions``/``classes``).
    """
    try:
        from coderadar._core import graph_stats
        stats = graph_stats()
    except ImportError as e:
        raise NoExtension(str(e)) from None
    except RuntimeError as e:  # no graph loaded
        raise NoIndex(str(e)) from None
    if not any(stats.get(k, 0) for k in (kinds or ("modules",))):
        raise NoIndex("the loaded graph indexed nothing")


# ── Entity ids ────────────────────────────────────────────────────────────

def friendly_entity_id(entity_id: str) -> str:
    """Present a stored entity ID in a shell-friendly form.

    Stored IDs are already shell-friendly since plan 5.1
    (``path/to/file.py::name``); this stays as the presenter for ids that
    arrive from an older store or a hand-written script — it converts
    backslashes to forward slashes and drops the redundant ``./`` / ``.\\``
    prefix. It is idempotent and a no-op on the canonical form.
    """
    friendly = entity_id.replace('\\', '/')
    friendly = friendly.removeprefix('./')
    return friendly


def canonical_entity_id(entity_id: str) -> str:
    """Resolve an entity ID to its canonical in-graph form.

    The graph is always walked as `.` from the project root, so in-graph ids
    always carry that same relative prefix. What varies is what the caller
    sends: an absolute path read off a result, a slash-vs-backslash variant,
    or the shell-friendly form search displays. All of those are normalised
    here to the stored key — no stored key or reference is changed.
    """
    try:
        from coderadar._core import lookup_entity
    except ImportError:
        return entity_id
    if lookup_entity(entity_id):
        return entity_id

    candidates: list[str] = []

    # Absolute → relative (with ./ prefix and bare)
    if os.path.isabs(entity_id):
        try:
            rel = os.path.relpath(entity_id, os.getcwd())
            candidates.append('.' + os.sep + rel)
            candidates.append(rel)
        except ValueError:
            pass

    # Friendly / separator-variant forms. The stored key is
    # "{optional .<sep> prefix}{path in <sep>}::{name}", so normalise to a
    # slash base and try every {bare, prefixed} x {slash, backslash} combo.
    base = entity_id.replace('\\', '/')
    base = base.removeprefix('./')
    for sep, prefix in (('/', './'), ('\\', '.\\')):
        body = base.replace('/', sep)
        candidates.append(body)
        candidates.append(prefix + body)

    for c in candidates:
        if c and lookup_entity(c):
            return c
    return entity_id


def display_file(d: dict) -> str:
    """Best-effort file for an entity dict — never '?' when derivable.

    Rust bindings disagree on the key (`file_path` vs `file` vs `path`),
    and query rows may carry neither; the entity id always embeds the path
    as `<path>::<name>`, so derive from there as a last resort.
    """
    for k in ("file_path", "file", "path"):
        v = d.get(k)
        if v:
            return str(v)
    eid = str(d.get("id", d.get("entity_id", "")))
    if "::" in eid:
        return eid.split("::")[0]
    return "?"


def find_entity(entity_id: str) -> dict | None:
    """The entity for any accepted id spelling, or None."""
    try:
        from coderadar._core import lookup_entity
        return lookup_entity(canonical_entity_id(entity_id))
    except (ImportError, RuntimeError):
        return None


def _text_search(query: str, top_k: int, kind: str | None = None) -> list[dict]:
    try:
        from coderadar._core import search_entities
        return search_entities(query, top_k, kind) or []
    except ImportError:
        return []


def _callers(entity_id: str) -> list[dict]:
    try:
        from coderadar._core import callers_of
        return callers_of(entity_id) or []
    except ImportError:
        return []


def _callees(entity_id: str) -> list[dict]:
    try:
        from coderadar._core import callees_of
        return callees_of(entity_id) or []
    except ImportError:
        return []


def suggest_entities(entity_id: str, limit: int = 3) -> list[dict]:
    """Near misses for an id that matched nothing.

    The core already resolves dotted qualified names; when even that misses,
    the last segment is usually enough to find what the caller meant.
    `search_entities` matches exact/prefix/contains, so a typo finds nothing
    at full length: `rendr` needs the probe shortened to `ren` before
    `render` shows up.
    """
    hint = entity_id.split("::")[-1].rsplit(".", 1)[-1]
    for length in range(len(hint), 2, -1):
        hits = [
            hit for hit in _text_search(hint[:length], 5)
            if hit.get("name") and hit.get("id") != entity_id
        ]
        if hits:
            return hits[:limit]
    return []


def _entity_or_raise(entity_id: str) -> dict:
    entity = find_entity(entity_id)
    if not entity:
        raise NotFound(entity_id, suggest_entities(entity_id))
    return entity


# ── Read operations ───────────────────────────────────────────────────────

SEARCH_KINDS = ("function", "class", "type_alias", "constant", "module", "import")


def search(query: str, kind: str | None = None, top_k: int = 10) -> list[dict]:
    """Keyword search over entity names, signatures and docstrings.

    Tokens match independently (OR). `kind` narrows to one of
    :data:`SEARCH_KINDS`; at most 20 results.
    """
    _require_index()
    if not query.strip():
        raise InvalidRequest("Please provide a query to search for.")
    # Refuse unknown kinds like the core does — a garbage kind otherwise
    # reads as "no results".
    if kind and kind.lower() not in SEARCH_KINDS:
        raise InvalidRequest(
            f"Unknown kind `{kind}` (expected: {' | '.join(SEARCH_KINDS)}).")
    results = _text_search(query, min(top_k, 20))
    if kind:
        results = [r for r in results if r.get("kind") == kind or r.get("entity_type") == kind]
    return results[:top_k]


def node(entity_id: str, include_neighbors: bool = False) -> dict:
    """One entity's full record; with `include_neighbors`, plus its direct
    ``callers`` and ``callees`` lists."""
    _require_index()
    entity = _entity_or_raise(entity_id)
    if include_neighbors:
        entity = dict(entity, callers=_callers(entity_id), callees=_callees(entity_id))
    return entity


def affected(entity_id: str, max_depth: int = 5) -> dict:
    """Transitive callers (the blast radius), breadth-first.

    Returns ``{"entity", "max_depth", "depths": {depth: [entity, …]},
    "central_ids": [id, …]}``. Within each depth, entities are ordered by
    harmonic centrality (most depended-on first); ``central_ids`` names the
    top three overall. Depth is capped at 20.
    """
    _require_index()
    entity = _entity_or_raise(entity_id)

    tree: dict[int, list[dict]] = {}
    visited = {entity_id}
    queue: deque[tuple[str, int]] = deque([(entity_id, 0)])
    while queue:
        current_id, depth = queue.popleft()
        if depth >= min(max_depth, 20):
            continue
        for caller in _callers(current_id):
            cid = caller.get("id", "")
            if cid and cid not in visited:
                visited.add(cid)
                tree.setdefault(depth + 1, []).append(caller)
                queue.append((cid, depth + 1))

    # Triage ranking: within each depth, order by harmonic centrality so the
    # top of each group is what actually matters. Best-effort — an extension
    # without rank_by_centrality keeps BFS order.
    centrality: dict[str, float] = {}
    try:
        from coderadar._core import rank_by_centrality
        all_ids = [e.get("id", "") for group in tree.values() for e in group]
        if all_ids:
            centrality = dict(rank_by_centrality(all_ids))
    except ImportError:
        pass
    if centrality:
        tree = {
            d: sorted(es, key=lambda e: centrality.get(e.get("id", ""), 0.0), reverse=True)
            for d, es in tree.items()
        }
    central_ids = [
        eid
        for eid, score in sorted(centrality.items(), key=lambda kv: kv[1], reverse=True)[:3]
        if score > 0
    ]
    return {
        "entity": entity,
        "max_depth": max_depth,
        "depths": dict(sorted(tree.items())),
        "central_ids": central_ids,
    }


def module_children(module_id: str) -> dict:
    """A module's direct contents: ``{"module_id", "classes", "functions",
    "imports", "constants"}`` (each a list of ``{name, id, line}`` dicts).
    Module ids look like ``path/to/file.py::module``."""
    _require_index()
    if not module_id.strip():
        raise InvalidRequest("Please provide a module ID (e.g. 'src/main.py::module').")
    module_id = canonical_entity_id(module_id)
    try:
        from coderadar._core import module_children as _mc
        children = _mc(module_id)
    except Exception as e:  # noqa: BLE001 - the core reports a miss as an error
        raise NotFound(module_id, detail=f"Module `{module_id}` not found or error: {e}") from None
    out: dict[str, Any] = {"module_id": module_id}
    for category in ("classes", "functions", "imports", "constants"):
        out[category] = list(children.get(category, []) or [])
    return out


def resolve(name: str, limit: int = 5) -> dict:
    """Framework-aware reference resolution.

    A route path (``/users/:id``) finds its handlers; any other name goes
    through the framework resolvers (``UserService``, ``*Model``, ``*View``).
    Returns ``{"name", "mode": "route" | "reference", "results": [...]}``,
    each result carrying ``confidence`` (and ``resolved_by`` / ``route``).
    """
    _require_index()
    if not name.strip():
        raise InvalidRequest("Please provide a name or path to resolve.")

    def searcher(n: str, lim: int) -> list[dict]:
        return _text_search(n, lim)

    if name.startswith("/"):
        from coderadar.resolvers.resolution import resolve_route
        return {"name": name, "mode": "route",
                "results": resolve_route(name, searcher, limit=limit) or []}
    from coderadar.resolvers import ALL_RESOLVERS
    from coderadar.resolvers.resolution import resolve_reference
    return {"name": name, "mode": "reference",
            "results": resolve_reference(name, searcher, ALL_RESOLVERS, limit=limit) or []}


# ── Analyses ──────────────────────────────────────────────────────────────

def _run_engine(fn, *args):
    """Call a native analysis; argument errors become InvalidRequest, any
    other failure (a panic included) EngineError."""
    try:
        return fn(*args)
    except (ValueError, TypeError) as e:
        raise InvalidRequest(str(e)) from None
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(str(e)) from None
    except BaseException as e:
        if isinstance(e, (KeyboardInterrupt, SystemExit)):
            raise
        # Without this a native panic escapes every handler and, under MCP,
        # wedges the stdio session with no reply.
        raise EngineError(f"engine panic ({type(e).__name__}: {e})") from None


def dead_code(min_confidence: float = 0.6, include_test_reachable: bool = False,
              max_findings: int = 100) -> list[dict]:
    """Functions unreachable from any entry point, most safely deletable first.

    Each finding carries ``entity_id``, ``entity_name``, ``file``, ``line``,
    ``kind`` (unreachable | transitively-dead | test-only | rta-dead),
    ``tier``, ``score`` (confidence), ``removable_lines``, ``evidence`` and
    ``nearest_root_distance``. Ranked evidence, not proof: check
    :func:`affected` before deleting.
    """
    _require_index("functions")
    from coderadar._core import find_dead_code
    return _run_engine(find_dead_code, min_confidence, include_test_reachable, max_findings)


def get_smells(entity_id: str | None = None, rule_id: str | None = None,
               strictness: str = "normal") -> list[dict]:
    """Code-smell findings, optionally for one entity and/or one rule.

    `strictness` is ``strict`` | ``normal`` | ``loose``. Each finding carries
    ``rule_id``, ``severity``, ``entity_id``, ``entity_name``, ``message``
    and the metric ``signals`` that triggered it.
    """
    _require_index("functions", "classes")
    from coderadar._core import get_smells as _get_smells
    return _run_engine(_get_smells, entity_id, rule_id, strictness)


def find_clones(min_lines: int = 10, min_similarity: float = 0.8,
                max_groups: int = 100) -> list[dict]:
    """Clone groups (Types 1-3), largest first.

    Each group carries ``clone_type``, ``similarity``, ``confidence_tier``,
    an optional ``reason`` and ``instances`` (``entity_id``, ``file``,
    ``start_line``/``end_line``, ``span_start``/``span_end``).
    """
    _require_index("functions")
    from coderadar._core import find_clones as _find_clones
    return _run_engine(_find_clones, min_lines, min_similarity, max_groups)


def find_scaffolding(include_secrets: bool = False, max_findings: int = 100) -> list[dict]:
    """Scaffolding debt: comment markers, placeholder bodies, temp-file
    names and (opt-in, redacted) secrets.

    Findings carry ``kind``, ``file``, ``line``, ``label`` and ``snippet``.
    The last row (``kind == "scan-stats"``) is a coverage footer, not a
    finding. `max_findings` applies per kind.
    """
    _require_index("functions")
    from coderadar._core import find_scaffolding as _find_scaffolding
    return _run_engine(_find_scaffolding, include_secrets, max_findings)
