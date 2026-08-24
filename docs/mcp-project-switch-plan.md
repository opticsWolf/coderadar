# MCP Project Switching — Implementation Plan

Branch: `mcp-dev`

> **Status: implemented.** Commits `5360f95` (selector), `6b63016`
> (explicit-choice suppression), `7ae0645` (selector-based
> `_wrong_project`), `ad83b37` (`codegraph_set_project` + guidance), and
> the README update. E2E in `tests/mcp/test_set_project.py`.

## Goal

Replace "one project per server, fixed at launch" with "one project per
server, **switchable at runtime**":

- One new tool, `codegraph_set_project`, defines the served project folder.
- Every other tool verifies its `project_path` against that currently-set
  root — accepting anything inside it (nearest-marker resolution), refusing
  foreign projects while naming the escape hatch (`codegraph_set_project`).

Out of scope (deliberately): serving multiple projects simultaneously,
per-project graph registries, daemon architectures. The single
GLOBAL_GRAPH / single BackgroundIndex shape is unchanged.

## Design summary

| Concern | Mechanism |
|---|---|
| Root selection | Nearest `.coderadar`/`.coderadar.toml` at-or-above the asked path (`roots.find_marker`) |
| Current-root tracking | Existing `LazyRootRetry.resolved` (`mcp/lazy.py`) — already the single source of truth used by `_wrong_project` |
| Process move | Existing `adopt_project_root` chdir (`mcp/roots.py`) — keeps entity-id prefixes and cwd-relative helpers correct |
| Re-index | Existing `BackgroundIndex.restart(".")` (`mcp/startup.py`) — generation counter already prevents stale threads overwriting new state |
| Per-project config | Existing `activate_config(root)` (`config.py`) — `[mutation]` policy, excludes, embedding model follow the switched project |
| Mutation safety | Unchanged. `analyze(root)` sets `INDEXED_ROOT` (`lib.rs:734–736`), so write confinement follows the new root automatically |

No Rust changes required.

---

## Step 1 — Selector resolution in `roots.py`

**File:** `py_agent/src/coderadar/mcp/roots.py`

Add:

```python
def resolve_selector(project_path: str) -> Optional[ResolvedRoot]:
    """Resolve a tool-call `project_path` the way agents mean it.

    Walks up from the asked path looking for a marker (find_marker).
    Returns the ResolvedRoot whose path is the marker parent, or None when
    the path cannot be canonicalised at all. Marker=None means "accepted
    but unconfirmed" — callers say so out loud.
    """
```

Implementation: `_canonical(Path(project_path))` → `find_marker(resolved)` →
`ResolvedRoot(path=marker.parent or resolved, source="project_path",
marker=marker)`.

Unit tests (new class in `tests/mcp/test_root_resolution.py`):
- file path inside a marked project → resolves to project root
- subdirectory inside a marked project → resolves to project root
- unmarked directory → returns root with `marker=None`
- nonexistent/garbage path → returns None

## Step 2 — Explicit-choice suppression in `lazy.py`

**File:** `py_agent/src/coderadar/mcp/lazy.py`

Add to `LazyRootRetry`:

```python
def mark_user_chosen(self, resolved: ResolvedRoot) -> None:
    """Record an explicit codegraph_set_project choice.

    Sets self.resolved and flags the retry as attempted so the client's
    roots/list answer can never override an explicit agent decision.
    """
```

Body: under lock, set `self._attempted = True`; set `self.resolved = resolved`.

Tests (extend `tests/mcp/test_lazy_roots.py`):
- after `mark_user_chosen`, `should_ask()` returns False
- `current().describe()` reflects the new root

## Step 3 — Selector-based `_wrong_project` in `server.py`

**File:** `py_agent/src/coderadar/mcp/server.py` (`_wrong_project`, line ~965)

Replace exact-match comparison:

```python
asked = Path(project_path).expanduser().resolve()
if asked == served:
    return None
```

with:

```python
selected = resolve_selector(project_path)
if selected is not None and selected.path == served:
    return None          # inside the served project (file, subdir, root…)
```

Refusal text changes: drop *"Start a second server…"*; replace with:

> Call `codegraph_set_project` with this path to switch this server to that
> project, or drop the `project_path` argument to keep asking about
> `{served}`.

Tests (`tests/mcp/test_project_path.py`):
- existing refusals still pass (foreign absolute paths)
- new: `project_path` = subdirectory of served root → proceeds
- new: `project_path` = file inside served root → proceeds
- new: refusal message mentions `codegraph_set_project`
  (`test_guidance_names_real_tools.py` enforces the name is registered —
  Step 5 must land before these tests run green)

## Step 4 — Switch handler

**File:** `py_agent/src/coderadar/mcp/server.py` (new private `_set_project`)
+ wiring in `create_server`.

Handler logic, in order:

1. `selected = resolve_selector(project_path)`; None → error naming the bad
   path.
2. If `not selected.confirmed and not confirm`: refuse, explaining no
   `.coderadar`/`.coderadar.toml` was found at or above the path, suggesting
   `coderadar init` there or re-calling with `confirm=true`.
3. Load config for the new root — mirror `cli._activate` semantics
   (`cli.py:94`): `activate_config(selected.path)`; a broken config logs a
   warning into the response and proceeds on defaults rather than failing
   the switch.
4. `adopt_project_root(selected)` (chdir).
5. `index.restart(".")` via `startup.current()`; if no handle installed
   (directly constructed server / tests), skip silently — same convention as
   `ensure_ready`.
6. `retry.mark_user_chosen(selected)` via `lazy.current()`; None-tolerant.
7. Return a report: new root, how it was confirmed, config warnings, and
   "indexing in the background — tool calls will wait for it (or report
   progress)".

Concurrency note (document in the tool description): a switch replaces the
graph wholesale; results of calls racing a switch are last-writer-wins, the
same semantics as the existing lazy retarget.

## Step 5 — Register the `codegraph_set_project` tool

**File:** `py_agent/src/coderadar/mcp/server.py`

Signature:

```python
@mcp.tool(description=(
    "Switch this server to a different project. All other tools then verify "
    "their project_path against this root. Pass any directory, subdirectory "
    "or file inside the target project — the nearest .coderadar marker at or "
    "above it defines the root. Indexing restarts in the background."
), annotations={...read_only_hint: False, destructive_hint: False,
                idempotent_hint: False, open_world_hint: True})
def codegraph_set_project(project_path: str, confirm: bool = False) -> str:
```

This is tool #19. Update:
- `tests/mcp/test_project_path.py::test_every_tool_offers_project_path` —
  `len(tools) == 18` → 19. Note: `codegraph_set_project` itself should NOT
  take a `project_path` parameter (its argument *is* the project); the test
  asserting every tool has `project_path` needs a carve-out list
  `{"codegraph_set_project"}`.
- Any hardcoded tool count in `server.py` instructions/docstring ("18 tools"
  appears in `cli.py` serve docstring) → 19.

## Step 6 — Guidance text sweep

All agent-facing text that describes the one-project model:

- `server.py` module docstring (~line 104–130): "One project per server"
  paragraph → "one project at a time; switch with `codegraph_set_project`".
- `_no_index_message()` (`server.py:~1000`): the "restart the server with
  --path" advice gains "or call `codegraph_set_project`".
- `cli.py::serve` docstring and the stderr startup line.
- `README.md`: MCP Server section bullet "**One project per server**" →
  describe runtime switching.

Run `tests/mcp/test_guidance_names_real_tools.py` — it validates every tool
name mentioned anywhere against the registered set, so typos in the new text
fail fast.

## Step 7 — End-to-end tests

New file `tests/mcp/test_set_project.py`. Pattern: two tmp projects, each
with a marker, a couple of Python files and its own `.coderadar.toml`;
`BackgroundIndex(analyze=<real coderadar.analyze>)`.

Cases:
1. **Happy path** — set_project(B) → explore/query answers contain B's
   entities only; A's entity ids gone.
2. **Unconfirmed root** — set_project(unmarked_dir) without `confirm` →
   refusal; with `confirm=true` → accepted, response says unconfirmed.
3. **Selector inputs** — pass a file and a subdir inside B; both land on B.
4. **Lazy-retry suppression** — after explicit switch, simulate middleware
   retry: `should_ask()` False, roots/list never consulted.
5. **Config follows** — B's `.coderadar.toml` sets a distinctive valid knob;
   assert `get_config()` reflects it post-switch.
6. **Mutation confinement** — after switching to B, `plan_body_replacement`
   targeting a path under A is rejected by policy (INDEXED_ROOT followed the
   switch).
7. **In-flight honesty** — call a read tool immediately after switch →
   either waits (ensure_ready) or reports progress; never answers from A's
   half-dead graph.

## Step 8 — Full suite + version bump

- `uv run pytest tests/` (expect 607 Python tests + the new ones green),
  plus Rust untouched: `cargo test` sanity run.
- Version bump decision: patch (0.7.2) vs minor (0.8.0) — new tool +
  behaviour change suggests **0.8.0**, aligning with the open
  v0.8-improvement-plan.md.

## Suggested commit sequence

| # | Commit | Contains |
|---|--------|----------|
| 1 | `roots: nearest-marker selector resolution` | Step 1 + tests |
| 2 | `lazy: record explicit project choices` | Step 2 + tests |
| 3 | `server: project_path resolves through markers` | Step 3 + tests |
| 4 | `mcp: codegraph_set_project tool` | Steps 4–6, count/guidance updates, E2E tests |
| 5 | `docs: runtime project switching` | README, docs cross-refs |

Each commit keeps the suite green (commit 3's new refusal-text assertions
are written to tolerate the pre-tool wording until commit 4 lands, or simply
fold 3+4 together if cleaner).

## Risks / notes

- **Windows paths**: `resolve_selector` inherits `Path.resolve()` +
  UNC handling already proven in `uri_to_path`; the E2E tests run on the
  dev machine (Windows) so drive-letter casing surprises will surface there.
  Compare resolved roots with `Path.__eq__` (case-preserving) — consider
  `os.path.normcase` for the equality check if Windows casing bites.
- **chdir is process-global**: a switch mid-session moves the cwd for every
  connection if a host multiplexes sessions onto one server process. Today's
  lazy retarget has the same property; acceptable, worth one line in the
  tool description.
- **Config failure mid-switch**: proceed-on-defaults matches CLI behaviour,
  but the response must carry the warning — silent default policy on a
  *writable* root would undercut the mutation story.
