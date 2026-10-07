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
import re
from collections import deque
from collections.abc import Sequence
from pathlib import Path
from typing import Any

__all__ = [
    "OPERATIONS",
    "SEARCH_KINDS",
    "TRAVERSE_DIRECTIONS",
    "EngineError",
    "InvalidRequest",
    "MissingDependency",
    "MutationFailed",
    "NoEmbeddings",
    "NoExtension",
    "NoIndex",
    "NotFound",
    "OpError",
    "affected",
    "as_of",
    "callees",
    "callers",
    "canonical_entity_id",
    "canonical_file_path",
    "compute_embeddings",
    "create_entity",
    "dead_code",
    "diagnose",
    "display_file",
    "explore",
    "find_clones",
    "find_entity",
    "find_scaffolding",
    "friendly_entity_id",
    "get_smells",
    "module_children",
    "node",
    "normalize_query_spelling",
    "parse_names",
    "query",
    "read_source",
    "reindex",
    "rename",
    "render_entity_code",
    "replace_body",
    "resolve",
    "resolve_names",
    "search",
    "search_similar",
    "stale_files",
    "status",
    "suggest_entities",
    "traverse",
    "update_file",
    "update_signature",
]

#: Every shared operation, by its one name. The MCP tool is
#: ``coderadar_<op>``, the CLI command ``<op>`` with hyphens, the
#: `CodeGraph` method ``<op>``. `set_project` has no function here: the MCP
#: server switches projects, the CLI takes ``-C``, and a `CodeGraph` is
#: bound to the project it was loaded for.
OPERATIONS = (
    "explore", "node", "search", "affected", "resolve", "query",
    "search_similar", "compute_embeddings", "module_children", "as_of",
    "traverse", "get_smells", "dead_code", "find_clones", "find_scaffolding",
    "replace_body", "update_signature", "rename", "create_entity",
    "reindex", "update_file", "status", "set_project",
    "callers", "callees", "diagnose",
)


# ── Errors ────────────────────────────────────────────────────────────────

class OpError(Exception):
    """An answer for the caller, not a crash: each surface words it."""


class NoExtension(OpError):
    """The native `coderadar._core` extension is not importable."""


class NoIndex(OpError):
    """No graph is loaded, or the loaded one indexed nothing."""


class InvalidRequest(OpError):
    """The arguments cannot be answered (empty query, unknown kind, …)."""


class TemporalUnsupported(InvalidRequest):
    """A `Snapshot` method with no temporal path refused honestly (§0.1a).

    A subclass of `InvalidRequest` so existing MCP/CLI error mapping keeps
    working: the request is well-formed but cannot be answered as-of-T.
    The message always names the supported alternative (`Snapshot.find`,
    downstream `Snapshot.traverse`, or the present-tense `CodeGraph`
    method). Distinct from a data answer: `find` returns `None` for
    "not in the graph at T"; this raises for "cannot be looked up at T".
    """


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


class MissingDependency(OpError):
    """An optional dependency the operation needs is not installed."""


class NoEmbeddings(OpError):
    """Semantic search found no embeddings and could not compute them."""


class MutationFailed(OpError):
    """An edit could not be planned or applied; nothing was written."""


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


def callers(entity_id: str) -> list[dict]:
    """Direct callers of `entity_id` (reverse call index).

    The shared seam behind CLI `callers`, MCP `coderadar_callers` and
    `CodeGraph.callers`: all three reach the same `_core.callers_of`
    backend, so the surfaces cannot disagree. Empty means callerless
    *or* unknown — pair with `find_entity` to tell them apart (R2-16).
    """
    return _callers(entity_id)


def callees(entity_id: str) -> list[dict]:
    """Direct callees of `entity_id` (forward call index).

    The shared seam behind CLI `callees`, MCP `coderadar_callees` and
    `CodeGraph.callees`; see `callers` for the contract.
    """
    return _callees(entity_id)


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

SEARCH_KINDS = ("function", "class", "type_alias", "constant", "module", "import",
                "route")


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


def search_symbols(query: str, top_k: int = 10, raw: bool = False) -> list[dict]:
    """FTS5 keyword search over concept text (§1.10, DR-34).

    Finds symbols *mentioning* X (names, docstrings, file paths, member
    lists — everything in the v2 concept JSON), ranked best-first by BM25
    (`rank` is negative, ascending). Returns `[{id, rank}]`; hydrate via
    :func:`node`. Zero storage change: the ledger's trigger-maintained
    `concepts_fts` index already covers every concept.

    Escaped by default: hostile input (`cats not dogs`, unbalanced quotes)
    degrades to safe matches, never an error. `raw=True` passes the FTS5
    MATCH expression through for power syntax. Live-only (retired concepts
    never match); concept bodies stay in blobs, unindexed. Empty query →
    `[]`. `top_k` clamps to [1, 50] in the engine.
    """
    _require_index()
    try:
        from coderadar._core import search_symbols as _search_symbols
        return [
            {"id": hit["id"], "rank": hit["rank"]}
            for hit in _search_symbols(query, top_k, raw)
        ]
    except ImportError as e:
        raise NoExtension(str(e)) from None
    except RuntimeError as e:
        # No graph loaded → NoIndex; a *storeless* graph → InvalidRequest
        # (the request needs a ledger to read, unlike every projection
        # query). The engine's message is pinned in `search_symbols`
        # (lib.rs); this substring is the contract between the layers.
        if "stored graph" in str(e):
            raise InvalidRequest(str(e)) from None
        raise NoIndex(str(e)) from None


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
                "results": resolve_route(name, searcher, _callees, limit=limit) or []}
    from coderadar.resolvers import ALL_RESOLVERS
    from coderadar.resolvers.resolution import resolve_reference
    return {"name": name, "mode": "reference",
            "results": resolve_reference(name, searcher, ALL_RESOLVERS, limit=limit) or []}


# ── Explore ───────────────────────────────────────────────────────────────
# Language spelling normalization adapted from CodeGraph's
# normalizeQuerySpelling (MIT License, https://github.com/colbymchenry/codegraph)

_ERLANG_ARITY_RE = re.compile(r'\b([A-Za-z_][\w@]*)/(\d{1,3})\b')
_ERLANG_MODULE_RE = re.compile(
    r'(^|[\s,()[\]])(?!(?:kind|lang|language|path|name):)'
    r'([A-Za-z_][\w@]*):([A-Za-z_][\w@]*)(?=$|[\s,()\]])'
)


def normalize_query_spelling(query: str) -> str:
    """Normalize language-native query spellings into index-compatible forms.

    Transforms so agent queries using language-native notation match the index:
      - Elixir/Erlang arity: ``fn/3`` → ``fn``
      - Elixir/Erlang module: ``mod:fn`` → ``mod.fn``

    Safe cross-language: Lua ``t:m`` maps to ``t.m``, and no other supported
    language uses a bare single-colon identifier pair.
    """
    # Strip arity tails: fn/3 → fn
    query = _ERLANG_ARITY_RE.sub(r'\1', query)
    # Module:function → module.function (preserving kind:/lang: prefixes)
    query = _ERLANG_MODULE_RE.sub(r'\1\2.\3', query)
    return query


def parse_names(query: str, symbols: Sequence[str] | None = None) -> list[str]:
    """Parse query string or explicit symbols into candidate names.

    Applies language spelling normalization so agent queries using
    language-native notation match the index.
    """
    if symbols:
        return [s.strip() for s in symbols if s.strip()]
    if not query.strip():
        return []
    # Normalize language spellings: Elixir fn/3→fn, mod:fn→mod.fn
    query = normalize_query_spelling(query)
    parts = re.split(r'[,;\s]+', query)
    return [p.strip() for p in parts if p.strip() and len(p.strip()) > 1]


def resolve_names(names: Sequence[str]) -> list[dict]:
    """Resolve names to entities: an exact id/name first, else the top
    three search hits for it."""
    results: list[dict] = []
    seen: set[str] = set()
    for name in names:
        entity = find_entity(name)
        if entity and entity.get("id") not in seen:
            results.append(entity)
            seen.add(entity["id"])
            continue
        for c in _text_search(name, 3):
            if c.get("id") not in seen:
                results.append(c)
                seen.add(c["id"])
    return results


def read_source(entity: dict) -> str | None:
    """Read line-numbered source for an entity from disk."""
    file_path = entity.get("file_path")
    start_line = entity.get("start_line", 1)
    end_line = entity.get("end_line", start_line)
    if not file_path or not start_line:
        return None
    # F14: entity paths are canonical root-relative ids — resolve against
    # the indexed root, not the CWD.
    try:
        from coderadar.excludes import resolve_entity_path as _resolve
        file_path = _resolve(file_path)
    except ImportError:
        pass
    try:
        with open(file_path, encoding="utf-8", errors="replace") as f:
            all_lines = f.readlines()
    except OSError:
        return None
    si = max(0, start_line - 1)
    ei = min(len(all_lines), end_line)
    return "".join(f"{i + 1}\t{all_lines[i]}" for i in range(si, ei))


def stale_files(file_paths: Sequence[str]) -> list[dict]:
    """The given files modified on disk since the graph was last synced.

    Returns ``[{"path", "mtime"}, ...]``; empty when nothing is loaded.
    """
    stale: list[dict] = []
    try:
        from coderadar._core import graph_stats
        # The core sets `indexed_at` at each commit_projection.
        indexed_at = graph_stats().get("indexed_at", 0.0)
    except (ImportError, RuntimeError):
        # RuntimeError: no graph loaded yet — nothing to be stale against.
        return stale
    if not indexed_at:
        return stale
    for fp in file_paths:
        try:
            mtime = os.path.getmtime(fp)
            if mtime > indexed_at:
                stale.append({"path": fp, "mtime": mtime})
        except OSError:
            pass
    return stale


def explore(query: str = "", symbols: Sequence[str] | None = None,
            direction: str = "both", max_files: int = 8) -> dict:
    """Source and call paths for the named symbols, in one answer.

    `query` is symbol names or a question (split on whitespace, commas and
    semicolons); `symbols` names them explicitly instead. Each name resolves
    exactly, else to its top search hits.

    Returns ``{"names", "files": [{"file_path", "entities"}], "relationships",
    "stale_files"}``: up to `max_files` files, each entity carrying its
    line-numbered ``source``; relationships are ``{"kind": "caller" |
    "callee", "entity_id", "entity_name", "other_id", "other_name"}`` (five
    per direction per entity); ``stale_files`` lists referenced files edited
    since the last sync.
    """
    _require_index()
    names = parse_names(query, symbols)
    if not names:
        raise InvalidRequest("Please provide symbol names or a question to explore.")
    resolved = resolve_names(names)
    if not resolved:
        raise NotFound(" ".join(names))

    by_file: dict[str, list[dict]] = {}
    for entity in resolved:
        by_file.setdefault(entity.get("file_path", "unknown"), []).append(entity)
    files = [
        {"file_path": fp, "entities": [dict(e, source=read_source(e)) for e in ents]}
        for fp, ents in list(by_file.items())[:max_files]
    ]

    relationships: list[dict] = []
    for entity in resolved:
        eid = entity["id"]
        name = entity.get("name", eid)
        if direction in ("upstream", "both"):
            for c in _callers(eid)[:5]:
                relationships.append({
                    "kind": "caller", "entity_id": eid, "entity_name": name,
                    "other_id": c.get("id"), "other_name": c.get("name", c.get("id", "?")),
                })
        if direction in ("downstream", "both"):
            for c in _callees(eid)[:5]:
                relationships.append({
                    "kind": "callee", "entity_id": eid, "entity_name": name,
                    "other_id": c.get("id"), "other_name": c.get("name", c.get("id", "?")),
                })

    return {
        "names": names,
        "files": files,
        "relationships": relationships,
        "stale_files": stale_files(list(by_file)),
    }


# ── Traverse / query ──────────────────────────────────────────────────────

TRAVERSE_DIRECTIONS = ("downstream", "upstream", "both")
_DIRECTION_ALIASES = {"out": "downstream", "in": "upstream"}
_TO_CORE_DIRECTION = {"downstream": "out", "upstream": "in", "both": "both"}


def traverse(entity_id: str, direction: str = "both",
             edge_kinds: Sequence[str] | None = None, max_depth: int = 3) -> dict:
    """Breadth-first walk from `entity_id` along any edge kinds.

    `direction` is ``downstream`` (callees, imports, bases), ``upstream``
    (callers, importers, subclasses) or ``both``; ``out`` / ``in`` are
    accepted as aliases. `edge_kinds` is any of ``calls``, ``imports``,
    ``inherits`` (alias ``extends``), ``overrides``; None means all. Depth
    is capped at 10.

    Returns ``{"entity", "entity_id", "direction", "max_depth",
    "edge_kinds", "results", "unresolved"}``; ``results`` are entity rows
    with ``depth`` and ``edge_type``, ``unresolved`` counts targets the walk
    could not follow.
    """
    _require_index()
    if not entity_id.strip():
        raise InvalidRequest("Please provide an entity ID to traverse from.")
    direction = _DIRECTION_ALIASES.get(direction, direction)
    if direction not in TRAVERSE_DIRECTIONS:
        raise InvalidRequest(
            f"Unknown direction `{direction}` (expected: {' | '.join(TRAVERSE_DIRECTIONS)}).")
    entity = _entity_or_raise(entity_id)
    entity_id = canonical_entity_id(entity_id)
    kinds = list(edge_kinds) if edge_kinds else None
    depth = min(max_depth, 10)
    core_direction = _TO_CORE_DIRECTION[direction]

    from coderadar._core import traverse as _traverse
    try:
        raw = _traverse(entity_id, depth, kinds or [], core_direction, None)
    except ValueError as e:  # unknown edge kinds
        raise InvalidRequest(str(e)) from None
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(str(e)) from None

    # 2.3: surface silent truncation — count targets the walk could not follow.
    try:
        from coderadar._core import traverse_unresolved
        unresolved = traverse_unresolved(entity_id, depth, kinds or [], core_direction)
    except Exception:  # noqa: BLE001 - truncation count is best-effort, 0 means "unknown"
        unresolved = 0

    return {
        "entity": entity,
        "entity_id": entity_id,
        "direction": direction,
        "max_depth": depth,
        "edge_kinds": kinds,
        "results": raw if isinstance(raw, list) else [],
        "unresolved": unresolved,
    }


def query(query: str) -> list[dict]:
    """Run a graph query: ``<entity> [select ...] [where ...] [group by ...]
    [order by ...] [limit N]`` (see docs/query-language.md). Every row
    carries ``id``, ``file_path``, ``kind`` and ``parent_id``. A malformed
    query or unknown field raises InvalidRequest naming the problem."""
    if not query.strip():
        raise InvalidRequest("Please provide a query.")
    _require_index()
    from coderadar._core import query_graph
    try:
        return list(query_graph(query))
    except ValueError as e:
        raise InvalidRequest(str(e)) from None
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(str(e)) from None


# ── Embeddings ────────────────────────────────────────────────────────────

# Cached fastembed model for semantic search (lazy-loaded, reused across queries)
_EMBED_MODEL = None


def _embedding_model():
    """Lazily load and cache the fastembed model (avoid reload per query)."""
    global _EMBED_MODEL
    if _EMBED_MODEL is None:
        from fastembed import TextEmbedding

        from coderadar.embedding import embedding_settings
        model_name, _dimension = embedding_settings()
        _EMBED_MODEL = TextEmbedding(model_name=model_name)
    return _EMBED_MODEL


def compute_embeddings(model_name: str | None = None, batch_size: int = 32) -> dict[str, int]:
    """Compute and store embeddings for all indexable entities.

    Uses fastembed locally; unchanged entities are skipped by content hash.
    `model_name` None takes the configured model, which is also what the
    search path loads — a mismatch there silently breaks similarity.

    Returns ``{"generated", "cached", "total", "errors"}``.
    """
    _require_index()
    from .embedding import (
        EmbeddingDedup,
        EmbedTarget,
        compute_content_hash,
        embedding_settings,
    )

    configured_model, dimension = embedding_settings()
    dedup = EmbeddingDedup(model_name=model_name or configured_model,
                           dimension=dimension, batch_size=batch_size)
    targets: list[EmbedTarget] = []

    try:
        from coderadar._core import search_entities
        # Collect all embeddable entities across all kinds
        for kind in ("function", "class", "module", "import", "constant", "type_alias"):
            for entity in search_entities("", 10_000, kind):
                entity_id = entity.get("id", "")
                if not entity_id:
                    continue
                body = entity.get("signature", "") or entity.get("name", "") or ""
                targets.append(EmbedTarget(
                    entity_id=entity_id,
                    body=body,
                    content_hash=compute_content_hash(body.encode()),
                    kind=kind,
                ))
    except ImportError:
        return {"generated": 0, "cached": 0, "total": 0, "errors": 1}

    results = dedup.embed_batch(targets, db=None)
    cached = 0
    try:
        from coderadar._core import set_embeddings_bulk
    except ImportError:
        return {"generated": 0, "cached": 0, "total": len(targets), "errors": 1}

    # One call, one projection clone. Looping set_embedding cloned the
    # whole ProjectedGraph per entity — O(N²) on a project of any size.
    entries = []
    for target, vec in zip(targets, results):
        if vec is None:
            cached += 1
            continue
        entries.append((target.id, list(vec), target.content_hash))

    try:
        report = set_embeddings_bulk(entries)
    except RuntimeError:
        return {"generated": 0, "cached": cached,
                "total": len(targets), "errors": len(entries)}

    return {"generated": int(report.get("applied", 0)), "cached": cached,
            "total": len(targets), "errors": len(report.get("missing", []))}


def search_similar(query: str | Sequence[float], top_k: int = 10) -> list[dict]:
    """Semantic search: entities whose embedding is nearest the query.

    `query` is natural language (embedded locally with fastembed; at most
    20 results, and embeddings are computed on first use) or a ready
    embedding vector. Results carry ``similarity``, ``name``, ``kind`` and
    the file.
    """
    from coderadar._core import search_similar as _ss
    if not isinstance(query, str):
        return _ss(list(query), top_k)

    _require_index()
    if not query.strip():
        raise InvalidRequest("Please provide a natural-language query for semantic search.")
    try:
        embedding = list(next(iter(_embedding_model().embed([query]))))
    except ImportError:
        raise MissingDependency("fastembed") from None
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(f"Embedding failed: {e}") from None

    try:
        return _ss(embedding, min(top_k, 20))
    except RuntimeError:
        # No embeddings in the index yet — compute them once and retry.
        try:
            compute_embeddings()
            return _ss(embedding, min(top_k, 20))
        except Exception:  # noqa: BLE001 - auto-compute is best-effort, NoEmbeddings covers it
            raise NoEmbeddings("no embeddings found and computing them failed") from None


# ── Temporal ──────────────────────────────────────────────────────────────

def _graph():
    import coderadar
    return coderadar.CodeGraph()


def as_of(timestamp: str, query: str = "", symbols: Sequence[str] | None = None,
          *, graph: Any = None) -> dict:
    """Look symbols up as they were at a Macrame-canonical `timestamp`.

    Returns ``{"timestamp", "names", "entities": {name: entity | None},
    "predates_recorded_history": bool}``. Every name is reconstructed from
    the ledger at recorded-time T via one batched fold — an entity value of
    `None` means "not in the graph at T" (never asserted, not yet asserted,
    or already retired then), and `predates_recorded_history` is true when T
    sits before the first recorded write. `query` text contributes candidate
    names only; there is no temporal query execution (use `coderadar_query`
    for present-tense search). Bytes-at-T is §0.1(b) work: entities carry
    `ByteSpan`s, not source bodies, until the blob read path lands.
    """
    _require_index()
    if not timestamp:
        raise InvalidRequest(
            "Please provide an ISO 8601 timestamp (e.g. '2025-01-15T10:00:00Z').")
    # R2-10 + §0.1(a): one normalization truth (Macrame canonical UTC) for
    # every temporal entry point — `datetime.fromisoformat` used to accept
    # offsets/naive stamps Macrame then rejects downstream.
    try:
        from coderadar._core import lookup_entities_at as _lookup_at
        from coderadar._core import normalize_timestamp as _normalize_ts
        normalized = _normalize_ts(timestamp)
    except ImportError as e:
        raise EngineError(f"native extension unavailable: {e}") from e
    except ValueError as e:
        raise InvalidRequest(str(e)) from None
    # Validation parity gate: the same timestamp must pass `CodeGraph.as_of`
    # (MCP/CLI/Python agree by construction — same normalizer, same fold).
    try:
        (graph or _graph()).as_of(normalized)
    except InvalidRequest:
        raise
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(str(e)) from None
    names = parse_names(query, symbols)
    if not names:
        # Guidance path (render.as_of's empty-names branch): no symbols to
        # place in history, so no fold — and no store required. `predates`
        # is vacuously False: nothing was looked up.
        return {
            "timestamp": normalized,
            "names": [],
            "entities": {},
            "predates_recorded_history": False,
        }
    try:
        batch = _lookup_at(names, normalized)
    except ValueError as e:  # already normalized; defensive
        raise InvalidRequest(str(e)) from None
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(str(e)) from None
    found = batch.get("entities", {})
    return {
        "timestamp": normalized,
        "names": names,
        "entities": {name: found.get(name) for name in names},
        "predates_recorded_history": bool(batch.get("predates_recorded_history", False)),
    }


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


def diagnose(unresolved: bool = True, low_confidence: bool = True) -> dict:
    """Graph self-health: unresolved call targets + ambiguous base classes.

    The shared seam behind CLI `diagnose`, MCP `coderadar_diagnose` and
    `CodeGraph.diagnose`. Returns `{"unresolved": [{"id", "targets"}],
    "ambiguous_bases": [...], "ambiguous_base_count": int}` — an empty
    list reads as a clean bill of health, never as a report never written.
    Unresolved targets are attributed per function (R2-12), most gaps first.
    """
    out: dict[str, Any] = {"unresolved": [], "ambiguous_bases": [],
                           "ambiguous_base_count": 0}
    try:
        _require_index("functions")
    except NoIndex:
        # An indexed-nothing graph reports clean (headers + none),
        # the pre-ops CLI behaviour — not an error.
        return out
    try:
        from coderadar._core import (
            index_edge_stats,
            search_entities,
            unresolved_targets,
        )
    except ImportError as e:
        raise NoExtension(str(e)) from None
    if unresolved:
        rows = []
        for fn in search_entities("", 1000, "function"):
            names = ", ".join(unresolved_targets(fn["id"]))
            if names:
                rows.append({"id": fn["id"], "targets": names})
        rows.sort(key=lambda r: -len(r["targets"]))
        out["unresolved"] = rows
    if low_confidence:
        stats = index_edge_stats()
        out["ambiguous_bases"] = list(stats.get("ambiguous_base_details") or [])
        out["ambiguous_base_count"] = int(stats.get("ambiguous_bases", 0))
    return out


def store_repair(db_path: str | Path | None = None, delete: bool = False) -> dict:
    """Report load-blocking rows and retire what is safely retireable.

    The shared seam behind CLI `store-repair` (which also owns `--delete`).
    Deliberately NOT an MCP tool: re-analyze already retires v1 leftovers
    automatically, so an agent with an unloadable store should reindex, not
    hand-repair ledger rows — and `--delete` removes the whole store, a
    local-admin act no agent surface gets. Returns `{"v1_found",
    "v1_retired", "edges_retired", "unreadable_live", "legacy_ids"}`
    or `{"deleted": path}` / `{"missing": path}`.
    """
    from pathlib import Path as _Path

    p = _Path(db_path) if db_path else _Path(".coderadar/store/coderadar.db")
    if not p.exists():
        return {"missing": str(p)}
    if delete:
        p.unlink()
        return {"deleted": str(p)}
    try:
        from coderadar._core import store_repair as _repair
    except ImportError as e:
        raise NoExtension(str(e)) from None
    try:
        rep = _repair(str(p))
    except Exception as e:  # noqa: BLE001 - _core errors surface as EngineError
        raise EngineError(str(e)) from None
    out = {
        "v1_found": rep.get("v1_found", 0),
        "v1_retired": rep.get("v1_retired", 0),
        "edges_retired": rep.get("edges_retired", 0),
        "unreadable_live": rep.get("unreadable_live", 0),
        "legacy_ids": rep.get("legacy_ids", 0),
    }
    return out


def open_project(path: str | Path, confirm: bool = False,
                 ensure: bool = True) -> dict:
    """Resolve, enter and ensure a project — the shared project opener.

    Behind MCP `coderadar_set_project` (with `ensure=False`: the server
    owns its background index handle and restarts it) and CLI `-C`
    (via `_enter_project`, `confirm=True`, `ensure=False`: commands ensure
    their own graph, and an unmarked cwd is served as today). Direct
    callers get the full sequence: walk to the `.coderadar` marker, chdir,
    activate the TOML, cold-start-or-index.

    Returns `{root, marker, confirmed, source, config_ignored,
    config_error, stats}` where `source` is the ladder rung that chose the
    root (`"already"` when this root is already served — no rebuild),
    and `stats` is the post-ensure `graph_stats` (or `{}` when
    `ensure=False`). Raises `InvalidRequest` for an unusable path or an
    unmarked root without `confirm`; a broken TOML never fails the open —
    it is reported in `config_error` and defaults stand (the MCP
    warn-and-continue rule).
    """
    from coderadar.mcp.roots import adopt_project_root, resolve_selector

    selected = resolve_selector(str(path))
    if selected is None:
        raise InvalidRequest(
            f"`{path}` names no readable directory, so no project can be "
            "selected from it. Pass a directory (or any file inside one) "
            "and retry.")
    if not selected.confirmed and not confirm:
        raise InvalidRequest(
            "No `.coderadar/` or `.coderadar.toml` was found at or above "
            f"`{selected.path}`, so nothing confirms that directory as a "
            "project root. Run `coderadar init` there if it should be one, "
            "or re-call with confirm=true to serve it anyway.")
    root = str(selected.path)
    # Already serving = the process is already rooted there (the pre-ops
    # rule compared the served root, not the index: staying put rebuilds
    # nothing even with no index yet — `reindex` builds on demand).
    import os as _os
    if _os.path.normcase(str(Path(root).resolve())) == _os.path.normcase(
            str(Path.cwd().resolve())):
        stats: dict[str, Any] = {}
        try:
            from coderadar._core import graph_stats as _stats0
            stats = dict(_stats0())
        except Exception:  # noqa: BLE001 - no index yet: empty stats
            pass
        return {
            "root": root,
            "marker": selected.marker.name if selected.marker else None,
            "confirmed": selected.confirmed,
            "source": "already",
            "config_ignored": [],
            "config_error": None,
            "stats": stats,
        }
    ignored: list[str] = []
    config_error: str | None = None
    try:
        from coderadar.config import activate_config as _activate_cfg
        activated = _activate_cfg(Path(root))
        ignored = list(activated.ignored)
    except Exception as e:  # noqa: BLE001 - broken config: defaults stand
        config_error = f"{type(e).__name__}: {e}"
    adopt_project_root(selected)
    stats: dict[str, Any] = {}
    if ensure:
        try:
            from coderadar import coldstart as _coldstart
            from coderadar._core import graph_stats as _stats2
            _coldstart.build_graph(root)
            stats = dict(_stats2())
        except Exception as e:  # noqa: BLE001 - ensure failure is honest
            raise EngineError(f"project opens but the index will not build: {e}") from e
    return {
        "root": root,
        "marker": selected.marker.name if selected.marker else None,
        "confirmed": selected.confirmed,
        "source": selected.source,
        "config_ignored": ignored,
        "config_error": config_error,
        "stats": stats,
    }


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


# ── Edits ─────────────────────────────────────────────────────────────────
# Each edit plans first; with dry_run=False the plan is applied, which
# writes the file and updates the graph together. The result is
# ``{"plan": MutationPlan, "result": MutationResult | None, "note": str}``;
# ``result`` is None for a dry run. Any failure raises MutationFailed with
# the engine's message — nothing was written.

def _edit(plan_fn, dry_run: bool, graph: Any, note: str = "") -> dict:
    try:
        plan = plan_fn()
        result = None if dry_run else graph.apply(plan)
    except OpError:
        raise
    except Exception as e:  # noqa: BLE001 - surfaced as MutationFailed
        raise MutationFailed(str(e)) from None
    return {"plan": plan, "result": result, "note": note}


def replace_body(entity_id: str, new_body: str, expected_hash: str | None = None,
                 dry_run: bool = True, *, graph: Any = None) -> dict:
    """Replace a function/method body (not its signature or decorators).

    The replacement is re-based to the body's column, so it may be passed
    unindented or copied verbatim. `expected_hash` refuses the edit when
    the current body no longer matches.
    """
    _require_index()
    g = graph or _graph()
    return _edit(lambda: g.plan_replace_body(
        canonical_entity_id(entity_id), new_body, expected_hash, dry_run=True), dry_run, g)


def update_signature(entity_id: str, new_signature: str, inject_defaults: bool = False,
                     dry_run: bool = True, *, graph: Any = None) -> dict:
    """Change a function/method signature. The definition is rewritten;
    call sites come back as ``plan.unverified_sites`` for manual review."""
    _require_index()
    g = graph or _graph()
    return _edit(lambda: g.plan_update_signature(
        canonical_entity_id(entity_id), new_signature,
        inject_defaults=inject_defaults, dry_run=True), dry_run, g)


def rename(entity_id: str, new_name: str, dry_run: bool = True, *, graph: Any = None) -> dict:
    """Rename an entity at its definition and every reference."""
    _require_index()
    g = graph or _graph()
    return _edit(lambda: g.plan_rename(
        canonical_entity_id(entity_id), new_name, dry_run=True), dry_run, g)


def render_entity_code(
    language: str, kind: str, name: str, body: str, decorators: Sequence[str] | None,
    signature: str = "",
) -> str:
    """Render a source snippet for a new entity using language-aware syntax.

    `signature`, when given, is the complete function/method header to write
    verbatim (`fn f(a: T) -> U`, `def f(self) -> None`, …). The renderer only
    adds the language's body delimiter (the Python colon, the C-style braces,
    Ruby's `end`) so the agent can express full Rust/typed signatures that the
    name-only rendering never could (field session: `create_entity` could not
    express `fn sync_status_text(store: &Store) -> String`).
    """
    lang = (language or "").lower()
    kind_norm = (kind or "function").lower()
    body = (body or "").rstrip("\n")
    dec = "\n".join(decorators or [])
    dec_block = (dec + "\n") if dec else ""
    sig = (signature or "").strip()

    def indent(text: str, spaces: int = 4) -> str:
        pad = " " * spaces
        return "\n".join((pad + line) if line.strip() else line for line in text.split("\n"))

    if kind_norm in ("function", "method", "fn"):
        if sig:
            if lang in ("python", "py"):
                header = sig if sig.endswith(":") else sig + ":"
                inner = indent(body) or "    pass"
                return f"{dec_block}{header}\n{inner}\n"
            if lang in ("ruby", "rb"):
                return f"{dec_block}{sig}\n{body}\nend\n"
            # C-style block languages: rust, go, js/ts, java, csharp, php, …
            return f"{dec_block}{sig} {{\n{body}\n}}\n"
        if lang in ("python", "py"):
            return f"{dec_block}def {name}():\n{indent(body)}\n"
        if lang in ("rust", "rs"):
            return f"{dec_block}pub fn {name}() {{\n{body}\n}}\n"
        if lang == "go":
            return f"{dec_block}func {name}() {{\n{body}\n}}\n"
        if lang in ("javascript", "typescript", "js", "ts", "jsx", "tsx"):
            return f"{dec_block}function {name}() {{\n{body}\n}}\n"
        if lang in ("java",):
            return f"{dec_block}public void {name}() {{\n{body}\n}}\n"
        if lang in ("csharp", "cs"):
            return f"{dec_block}public void {name}() {{\n{body}\n}}\n"
        if lang in ("php",):
            return f"{dec_block}function {name}() {{\n{body}\n}}\n"
        if lang in ("ruby", "rb"):
            return f"{dec_block}def {name}\n{body}\nend\n"
        # generic brace language fallback
        return f"{dec_block}{name}() {{\n{body}\n}}\n"

    if kind_norm in ("class", "struct"):
        if lang in ("python", "py"):
            inner = indent(body) or "    pass"
            return f"{dec_block}class {name}:\n{inner}\n"
        if lang in ("ruby", "rb"):
            return f"{dec_block}class {name}\n{body}\nend\n"
        return f"{dec_block}class {name} {{\n{body}\n}}\n"

    if kind_norm in ("constant", "variable", "const", "var"):
        if lang in ("python", "py"):
            return f"{dec_block}{name} = {body or 'None'}\n"
        if lang == "go":
            return f"{dec_block}const {name} = {body or 'nil'}\n"
        if lang in ("javascript", "typescript", "js", "ts"):
            return f"{dec_block}const {name} = {body or 'null'};\n"
        return f"{dec_block}{name} = {body or 'null'}\n"

    # Unknown kind: emit the body verbatim
    return (body + "\n") if body else ""


def canonical_file_path(file_path: str) -> str:
    r"""Resolve a file path to the project-relative form the graph stores.

    Absolute paths become relative to the cwd (the project root), so
    create_entity's reindex step matches existing entities instead of
    creating duplicates.
    """
    if os.path.isabs(file_path):
        try:
            return '.' + os.sep + os.path.relpath(file_path, os.getcwd())
        except ValueError:
            return file_path
    if file_path.startswith(('./', '.\\')):
        return file_path
    return '.' + os.sep + file_path


def create_entity(file_path: str, language: str, kind: str, name: str, body: str,
                  decorators: Sequence[str] | None = None, anchor: str = "end",
                  signature: str | None = None, dry_run: bool = True,
                  *, graph: Any = None) -> dict:
    """Insert a new function, class or constant into `file_path`.

    The code is rendered from `name` / `body` / `decorators` per
    `language`; for function-like kinds `signature` is the complete header,
    written verbatim. `anchor` is ``end``, ``top`` or an entity id to insert
    after.
    """
    _require_index()
    g = graph or _graph()
    note = ""
    if (signature or "").strip() and kind.lower() not in ("function", "method", "fn"):
        note = (f"`signature` is only used for function-like kinds; it was "
                f"ignored for kind '{kind}'.")

    def plan():
        code = render_entity_code(language, kind, name, body, decorators, signature or "")
        if not code.strip():
            raise InvalidRequest("Cannot render entity: provide a non-empty body or kind.")
        anchor_norm = anchor or "end"
        if anchor_norm not in ("top", "end"):
            anchor_norm = canonical_entity_id(anchor_norm)
        return g.plan_create_entity(canonical_file_path(file_path), anchor_norm, code,
                                    dry_run=True)

    return _edit(plan, dry_run, g, note)


# ── Index lifecycle ───────────────────────────────────────────────────────

def reindex(with_embeddings: bool = False, full: bool = False,
            exclude: Sequence[str] | None = None, root: str | Path = ".") -> dict:
    """Bring the index for `root` up to date.

    By default the cheap way (v0.8 P2-4): a warm store loads and only the
    changed files are re-parsed; without a loadable store the whole tree is
    walked. `full` (or one-shot `exclude` patterns) always walks the whole
    tree. Returns ``{"stats", "embeddings"?, "embeddings_error"?}``.

    Step-4 surface: the root's `.coderadar.toml` is (re-)activated first,
    so MCP/API reindex picks up config edits exactly like the CLI (which
    `_activate`s per command). A missing or broken file keeps the current
    core config — reindex must never fail for a config problem.
    """
    try:
        from coderadar.config import activate_config as _activate_cfg
        _activate_cfg(Path(root))
    except Exception:  # noqa: BLE001 - config must not fail the reindex
        pass
    try:
        from coderadar._core import graph_stats
    except ImportError as e:
        raise NoExtension(str(e)) from None
    try:
        if full or exclude:
            import coderadar
            coderadar.analyze(str(root), exclude=list(exclude) if exclude else None)
        else:
            from coderadar import coldstart
            coldstart.build_graph(root)
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(str(e)) from None
    out: dict[str, Any] = {"stats": graph_stats()}
    # §1.9 (DR-30 notice): blob counts ride every reindex report — full
    # analyze resets and refills them; the cheap path accumulates the
    # updates it applied.
    try:
        import coderadar
        out["blobs"] = coderadar.blob_stats()
    except Exception:  # noqa: BLE001 - stats must not fail the report
        pass
    if with_embeddings:
        try:
            out["embeddings"] = compute_embeddings()
        except Exception as e:  # noqa: BLE001 - reported, the index itself is fine
            out["embeddings_error"] = str(e)
    return out


def update_file(file_path: str, content: str | None = None, *, graph: Any = None):
    """Sync one file into the graph: from `content`, else from disk; a file
    deleted from disk is dropped from the graph. Returns the
    `coderadar.UpdateReport` (``removed`` / ``entities_removed`` for a
    deletion, ``fully_applied`` False for a recovered parse)."""
    _require_index()
    if not file_path.strip():
        raise InvalidRequest("Please provide a file path.")
    try:
        return (graph or _graph()).update_file(file_path, content)
    except Exception as e:  # noqa: BLE001 - surfaced as EngineError
        raise EngineError(str(e)) from None


def status(root: str | Path | None = None) -> dict:
    """What is indexed for `root` (default: the cwd), and how fresh it is.

    Returns ``{"root", "config", "store", "loaded", "stats", "age_seconds",
    "store_fresh"}``: ``config`` / ``store`` are paths or None; ``stats``
    (the graph counts) and ``age_seconds`` are None when no graph is
    loaded; ``store_fresh`` says whether the store on disk is newer than
    every source file (None without a store).
    """
    import time

    root_path = Path(root or os.getcwd()).resolve()
    config = root_path / ".coderadar.toml"
    store = root_path / ".coderadar" / "store"
    out: dict[str, Any] = {
        "root": str(root_path),
        "config": str(config) if config.exists() else None,
        "store": str(store) if store.exists() else None,
        "loaded": False,
        "stats": None,
        "age_seconds": None,
        "store_fresh": None,
    }
    try:
        from coderadar import coldstart
        db = coldstart.store_db_path(root_path)
        if db is not None:
            out["store_fresh"] = bool(coldstart.store_is_fresh(root_path, db))
    except Exception:  # noqa: BLE001, S110 - freshness is best-effort
        pass
    try:
        from coderadar._core import graph_stats
        stats = graph_stats()
    except ImportError as e:
        raise NoExtension(str(e)) from None
    except RuntimeError:
        return out  # no graph loaded in this process
    out["loaded"] = True
    out["stats"] = stats
    indexed_at = stats.get("indexed_at")
    if indexed_at:
        out["age_seconds"] = max(0.0, time.time() - float(indexed_at))
    return out
