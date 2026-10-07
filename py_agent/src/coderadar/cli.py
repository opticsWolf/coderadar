"""CodeRadar v3.6 — Command-Line Interface (§16)"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import NoReturn

import click
from rich.console import Console
from rich.table import Table

from . import __version__

# soft_wrap: rich must not fold lines at its width guess (80 when piped);
# a folded entity id cannot be pasted back into the next command.
console = Console(soft_wrap=True)
# R2-15/R2-13: diagnostics (cold-start notes, fallbacks, config nudges)
# go to stderr, so stdout stays machine-readable (pipes, --format json).
class _LazyErrConsole:
    """`err_console` without the import-time stderr bind (step-4 surface).

    Importing `cli` must not touch stderr: test collectors and library
    importers get a pristine stream, and the first real diagnostic pays
    for the Console exactly once. Call sites are unchanged (`err_console`
    still answers `.print`).
    """
    _console = None

    def __getattr__(self, name):
        if type(self)._console is None:
            type(self)._console = Console(file=sys.stderr)
        return getattr(type(self)._console, name)


err_console = _LazyErrConsole()


# §1.3 (DR-10): framework extraction lives in `coderadar.framework` and
# runs inside `coderadar.analyze` (persisted routes, not init-only
# memory); star exports are applied by `analyze`/`load` themselves. Both
# former CLI-local passes are deleted, not moved.


def _activate(project_root) -> None:
    """Load `.coderadar.toml` for `project_root` and push it into the core.

    Called by every command that touches a project. Nothing read the file
    before this, so the whole documented config surface had no effect; a
    command that skips this call silently runs on defaults again.

    Keys the core cannot use are reported on stderr rather than swallowed —
    an inert knob that stays quiet is what this phase exists to remove.
    """
    from pathlib import Path

    from .config import activate_config
    try:
        activated = activate_config(Path(project_root))
    except Exception as exc:  # noqa: BLE001 - a broken config must not kill the command
        err_console.print(f"[yellow]Config not applied:[/yellow] {exc}")
        return
    if activated.ignored:
        err_console.print(
            f"[dim]Config: {len(activated.ignored)} setting(s) with no consumer "
            f"were ignored ({', '.join(activated.ignored[:3])}"
            f"{', ...' if len(activated.ignored) > 3 else ''})[/dim]"
        )


# v0.8 P2-4: the staleness rules live in coderadar.coldstart — the single
# implementation shared by the CLI's load/analyze ordering and the MCP
# background index's incremental cold start.
from .coldstart import (
    store_db_path as _store_db_path,
)
from .coldstart import (
    store_is_fresh as _store_is_fresh,
)


def _ensure_graph(path: str = "."):
    """Return a CodeGraph with an index behind it, restoring one if a
    store exists.

    The graph lives in the process that built it. `coderadar init` indexes
    and exits, so every read-only command that followed it started with an
    empty core. v0.8 P1: before re-indexing, the command tries a cold
    start from the Macrame ledger - `load_snapshot` rebuilds the in-memory
    graph from concept JSON v2 in milliseconds, instead of the full
    analyze the re-analysis did before.

    Order: reuse a matching loaded graph; else `_activate(path)` (the
    effective config for every command is the one in the project -
    BUGS_QUIRKS #3); else, if the store exists and is fresh, cold-start
    from it; else (or on any load error - this is also the v1 -> v2 store
    upgrade path) fall back to a full analyze. For an initialized project
    that analyze attaches the store automatically, so the re-index
    persists and the next command cold-starts.

    A loaded graph is only reused when it belongs to *this* directory: the
    core records the root each index walked, and a graph from somewhere
    else answers every question about this tree wrongly while looking
    completely healthy. Different root - or no graph - means restore or
    index here.
    """
    import coderadar
    from coderadar._core import graph_stats

    graph = coderadar.CodeGraph()
    try:
        stats = graph_stats()
        root = stats.get("indexed_root")
        if root:
            # std::fs::canonicalize emits Windows verbatim paths (\?\C:\...);
            # pathlib compares them as a different directory, so strip first.
            root_str = str(root).removeprefix("\\\\?\\")
            if Path(root_str).resolve() == Path(path).resolve():
                return graph
    except RuntimeError:
        pass

    _activate(path)

    root_path = Path(path).resolve()
    db_path = _store_db_path(root_path)
    if db_path is not None and _store_is_fresh(root_path, db_path):
        try:
            coderadar.load(str(db_path), str(root_path))
            err_console.print(
                "[dim]Cold start: graph restored from the Macrame store.[/dim]")
            return graph
        except Exception as exc:  # noqa: BLE001 - any load failure falls back to analyze
            err_console.print(
                f"[yellow]Store load failed ({exc}); falling back to a full "
                f"analyze (this also upgrades a v1 store).[/yellow]"
            )

    err_console.print("[dim]No graph for this directory - indexing...[/dim]")
    coderadar.analyze(path)
    return graph


@click.group()
@click.version_option(version=__version__, prog_name="coderadar",
                      message="coderadar %(version)s (spec v3.6)")
@click.option("-C", "--project", type=click.Path(exists=True, file_okay=False),
              default=None,
              help="Run in this project. Default: the nearest .coderadar marker "
              "at or above the current directory.")
@click.pass_context
def main(ctx: click.Context, project: str | None):
    """CodeRadar — live semantic graph of your codebase.

    Indexes symbols and call edges incrementally so agents and developers can
    query, explore and safely rewrite code. `coderadar mcp serve` exposes the
    same graph to MCP clients: every MCP tool `coderadar_<op>` is the
    command `coderadar <op>` here (underscores become hyphens), with
    `--format json` for scripts.
    """
    ctx.meta[_PROJECT_KEY] = str(Path(project).resolve()) if project else None
    # Piped output is read by programs that expect UTF-8; on Windows it
    # defaulted to the ANSI code page, which cannot encode most source text.
    for stream in (sys.stdout, sys.stderr):
        if not stream.isatty() and hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")


@main.command()
@click.argument("path", type=click.Path(exists=True), default=".")
@click.option("--force", is_flag=True, help="Overwrite an existing .coderadar.toml")
def init(path: str, force: bool):
    """Initialize CodeRadar in a project directory.

    Writes .coderadar.toml, creates .coderadar/store/, and runs the first
    analysis.
    """
    from datetime import datetime, timezone
    from pathlib import Path

    root = Path(path).resolve()
    coderadar_dir = root / ".coderadar"
    store_dir = coderadar_dir / "store"
    # The loader reads `.coderadar.toml` at the project root and nothing else.
    # This used to write `.coderadar/config.toml` in a schema no code had ever
    # parsed ([languages], [indexing], [mcp] — none of them exist), so an
    # edited setting went nowhere.
    config_file = root / ".coderadar.toml"

    if config_file.exists() and not force:
        console.print(f"[yellow]{config_file.name} already exists in {root}[/yellow]")
        console.print("Use --force to re-initialize.")
        return

    # Create directory structure
    coderadar_dir.mkdir(exist_ok=True)
    store_dir.mkdir(exist_ok=True)

    # F6/E2: --force on an existing project used to silently overwrite a
    # hand-tuned .coderadar.toml AND re-analyze into the same (possibly
    # poisoned) ledger. Back up the config and start the store from scratch
    # so neither the settings nor a v1-poisoned ledger survive.
    if force and config_file.exists():
        backup = root / ".coderadar.toml.bak"
        backup.write_text(config_file.read_text(encoding="utf-8"), encoding="utf-8")
        console.print(f"  Backed up existing config to {backup}")
    if force:
        store_db = store_dir / "coderadar.db"
        if store_db.exists():
            store_db.unlink()
            console.print(f"  Removed existing store {store_db} (fresh ledger)")

    # Write default config
    config_content = f'''# CodeRadar project configuration
# Generated by `coderadar init` on {datetime.now(timezone.utc).isoformat()}
#
# `coderadar reindex` prints a line naming any key it could not use, so a
# stale or misspelled setting here will say so rather than sit silent.

[project]
# Narrow the walk to these subdirectories; omitted, the whole root is walked.
# roots = ["src/", "tests/"]
exclude = ["**/__pycache__/**", "**/.venv/**", "**/node_modules/**"]

[database]
path = ".coderadar/store/coderadar.db"

[embedding]
# Index-time and query-time must name the same model: a dimension mismatch
# produces confident nonsense rather than an error.
model = "BAAI/bge-small-en-v1.5"
dimension = 384
batch_size = 32

[watch]
debounce_ms = 100
max_file_size_bytes = 1048576

[mutation]
enabled = true
default_dry_run = true
allow = ["src/", "lib/", "tests/", "scripts/"]
deny = [".git/", ".coderadar/", "/migrations/", "/*.lock", "/generated/"]

[resolution]
min_confidence = 0.3

[resolution.import_graph]
max_import_depth = 3
include_same_package = true
'''
    config_file.write_text(config_content, encoding="utf-8")

    # Add .coderadar/ to .gitignore if present
    gitignore = root / ".gitignore"
    coderadar_ignore = ".coderadar/"
    if gitignore.exists():
        content = gitignore.read_text()
        if coderadar_ignore not in content:
            with gitignore.open("a") as f:
                f.write(f"\n# CodeRadar\n{coderadar_ignore}\n")
    else:
        gitignore.write_text(f"{coderadar_ignore}\n")

    console.print(f"[green]OK  Initialized CodeRadar in {root}[/green]")
    console.print(f"  Config:    {config_file}")
    console.print(f"  Store:     {store_dir}")
    if gitignore.exists():
        console.print(f"  Gitignore: .coderadar/ added to {gitignore}")

    # Run initial analysis
    console.print("\n[bold]Running initial analysis...[/bold]")
    import coderadar
    graph = coderadar.analyze(str(root), create_store=True)
    stats = graph.stats()
    console.print(f"  Modules:    {stats.get('modules', 0)}")
    console.print(f"  Classes:    {stats.get('classes', 0)}")
    console.print(f"  Functions:  {stats.get('functions', 0)}")
    console.print(f"  Imports:    {stats.get('imports', 0)}")
    console.print(f"  Call edges: {stats.get('call_edges', 0)}")
    # §1.3 (DR-10): routes/handlers persisted by `analyze` above ride the
    # stats (star exports are applied inside `analyze`/`load` too).
    if stats.get("routes", 0):
        console.print(f"  Routes:     {stats['routes']}")
        console.print(f"  Handlers:   {stats.get('route_edges', 0)}")
    console.print("[green]OK  Analysis complete[/green]")


# ── Project, output and errors ───────────────────────────────────────────
#
# Every operation below is the MCP tool of the same name (hyphens for
# underscores): same `coderadar.ops` call, same `coderadar.render` text.
# `--format json` prints the op's return value instead.

_PROJECT_KEY = "coderadar.project"
APPLY_HINT = "re-run with `--apply`"


def _enter_project(*paths: str | None, project: str | None = None,
                   walk: bool = True) -> list[str | None]:
    """Move the process onto the project root; return `paths` re-expressed
    against it (relative when inside it, absolute otherwise).

    The root is `project` (legacy PATH arguments), else `-C/--project`,
    else the cwd — and with `walk`, the nearest `.coderadar` marker at or
    above that wins, so a command run from `src/deep/` works on the whole
    project. The ops resolve graph paths against the cwd, exactly as the
    MCP server does after its own root ladder.
    """
    absolute = [Path(p).resolve() if p else None for p in paths]
    if project is None:
        ctx = click.get_current_context(silent=True)
        project = ctx.meta.get(_PROJECT_KEY) if ctx is not None else None
    start = Path(project).resolve() if project else Path.cwd().resolve()
    if walk:
        # Shared opener (step-4 surface): marker walk, chdir, TOML
        # activation. `confirm=True` preserves the old rule (an unmarked
        # start dir is served, not gated); `ensure=False` because commands
        # ensure their own graph via `_ensure_graph`.
        from . import ops
        try:
            opened = ops.open_project(str(start), confirm=True, ensure=False)
        except ops.OpError as e:
            _fail(e)
        root = Path(opened["root"])
    else:
        root = start
        os.chdir(root)
    out: list[str | None] = []
    for p in absolute:
        if p is None:
            out.append(None)
            continue
        try:
            out.append(str(p.relative_to(root)))
        except ValueError:
            out.append(str(p))
    return out


def _json_default(value):
    import dataclasses
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return dataclasses.asdict(value)
    return str(value)


def _emit(fmt: str, result, text) -> None:
    """Print `result` as JSON, or as `text(result)` for people."""
    if fmt == "json":
        click.echo(json.dumps(result, indent=2, default=_json_default))
    else:
        click.echo(text(result))


def _fail(e) -> NoReturn:
    """Report an ops error on stderr; exit 2 for bad input, 1 otherwise."""
    from . import ops, render
    if isinstance(e, ops.NoExtension):
        msg = "The coderadar._core extension is not built."
    elif isinstance(e, ops.NoIndex):
        msg = "No index for this project. Run `coderadar init` to create one."
    elif isinstance(e, ops.NotFound):
        msg = e.detail or render.not_found(e.entity_id, e.candidates, "`coderadar search`")
    elif isinstance(e, ops.MutationFailed):
        msg = render.mutation_error(str(e), update_file="`coderadar update-file`")
    elif isinstance(e, ops.MissingDependency):
        msg = render.fastembed_missing(compute="`coderadar compute-embeddings`")
    elif isinstance(e, ops.NoEmbeddings):
        msg = render.no_embeddings(compute="`coderadar compute-embeddings`",
                                   reindex="`coderadar reindex --with-embeddings`")
    else:
        msg = str(e)
    click.echo(msg, err=True)
    raise SystemExit(2 if isinstance(e, ops.InvalidRequest) else 1)


def _op(fmt: str, call, text) -> None:
    """Load the graph, run one op, print its result."""
    from . import ops
    _ensure_graph()
    try:
        result = call()
    except ops.OpError as e:
        _fail(e)
    _emit(fmt, result, text)


def _renamed(old: str, new: str) -> None:
    click.echo(f"`coderadar {old}` is now `coderadar {new}`.", err=True)


def _text_arg(value: str | None) -> str | None:
    """'-' reads the value from stdin."""
    return sys.stdin.read() if value == "-" else value


def _format_option(f):
    return click.option(
        "--format", "fmt", type=click.Choice(["text", "json"]), default="text",
        show_default=True, help="text for people, json for scripts.")(f)


def _apply_option(f):
    return click.option(
        "--apply", is_flag=True,
        help="Write the change. Without it this is a dry run that prints the diff.")(f)


# ── Read ─────────────────────────────────────────────────────────────────

@main.command()
@click.argument("symbols", nargs=-1)
@click.option("--query", "-q", "query_text", default="",
              help="Free text naming the symbols, instead of or besides SYMBOLS.")
@click.option("--direction", type=click.Choice(["downstream", "upstream", "both"]),
              default="both", show_default=True)
@click.option("--max-files", default=8, show_default=True)
@_format_option
def explore(symbols: tuple, query_text: str, direction: str, max_files: int, fmt: str):
    """Source plus call paths for the named SYMBOLS (MCP coderadar_explore)."""
    from . import ops, render
    _enter_project()
    _ensure_graph()
    try:
        result = ops.explore(query_text, list(symbols), direction, max_files)
    except ops.InvalidRequest:
        click.echo(render.explore_usage(
            example='coderadar explore "User.save authenticate"'), err=True)
        raise SystemExit(2) from None
    except ops.NotFound:
        click.echo(render.explore_miss(
            ops.parse_names(query_text, list(symbols)),
            search="`coderadar search`",
            search_similar="`coderadar search-similar`"), err=True)
        raise SystemExit(1) from None
    except ops.OpError as e:
        _fail(e)
    _emit(fmt, result, render.explore)


@main.command()
@click.argument("entity_id")
@click.option("--neighbors", "include_neighbors", is_flag=True,
              help="Also list direct callers and callees.")
@_format_option
def node(entity_id: str, include_neighbors: bool, fmt: str):
    """One entity's details (MCP coderadar_node)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.node(entity_id, include_neighbors), render.node)


@main.command()
@click.argument("query_text")
@click.option("--kind", default=None,
              help="function | class | type_alias | constant | module | import")
@click.option("--top-k", default=10, show_default=True)
@_format_option
def search(query_text: str, kind: str | None, top_k: int, fmt: str):
    """Find symbols by keyword (MCP coderadar_search)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.search(query_text, kind, top_k),
        lambda rows: render.search(
            query_text, kind, rows, explore="`coderadar explore`",
            search_similar="`coderadar search-similar`"))


@main.command()
@click.argument("entity_id")
@click.option("--max-depth", default=5, show_default=True)
@_format_option
def affected(entity_id: str, max_depth: int, fmt: str):
    """Transitive callers of ENTITY_ID, the blast radius (MCP coderadar_affected)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.affected(entity_id, max_depth),
        lambda result: render.affected(entity_id, result))


@main.command()
@click.argument("name")
@click.option("--limit", default=5, show_default=True)
@_format_option
def resolve(name: str, limit: int, fmt: str):
    """Framework references: routes (`/users/:id`), `*Model` / `*View` names
    (MCP coderadar_resolve)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.resolve(name, limit),
        lambda result: render.resolve(result, search="`coderadar search`"))


@main.command()
@click.argument("query_string")
@click.option("--format", "fmt", type=click.Choice(["text", "table", "json"]),
              default="text", show_default=True,
              help="text for people, table for a wide terminal, json for scripts.")
def query(query_string: str, fmt: str):
    """Run a graph query (MCP coderadar_query).

    Example: `functions where caller_count == 0`; the language is in
    docs/query-language.md."""
    from . import ops, render
    _enter_project()
    _ensure_graph()
    try:
        rows = ops.query(query_string)
    except ops.OpError as e:
        _fail(e)
    if fmt == "table":
        _print_query_table(query_string, rows)
    else:
        _emit(fmt, rows, lambda r: render.query(query_string, r, limit=None))


def _print_query_table(query_string: str, results: list[dict]) -> None:
    if not results:
        console.print("[yellow]No results[/yellow]")
        return
    # A pipe has no width: rich squeezed every column to a few characters.
    # Render wide instead; identity columns never wrap (grep-able).
    if sys.stdout.isatty():
        tbl_console = console
    else:
        tbl_console = Console(file=sys.stdout, width=250)
    table = Table(title=f"Query: {query_string}")
    for key in results[0]:
        if key in ("id", "name", "entity_id"):
            table.add_column(key, style="cyan", no_wrap=True)
        else:
            table.add_column(key, style="cyan", overflow="fold")
    for row in results:
        table.add_row(*[str(row.get(k, "")) for k in results[0]])
    tbl_console.print(table)
    tbl_console.print(f"[dim]{len(results)} result(s)[/dim]")


@main.command(name="search-similar")
@click.argument("query_text")
@click.option("--top-k", default=10, show_default=True)
@_format_option
def search_similar(query_text: str, top_k: int, fmt: str):
    """Semantic search; embeddings are computed on first use
    (MCP coderadar_search_similar)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.search_similar(query_text, top_k),
        lambda rows: render.search_similar(query_text, rows))


@main.command(name="compute-embeddings")
@click.option("--model", "model_name", default=None,
              help="fastembed model (default: [embeddings] model in .coderadar.toml).")
@click.option("--batch-size", default=32, show_default=True)
@_format_option
def compute_embeddings(model_name: str | None, batch_size: int, fmt: str):
    """Compute embeddings for every indexed entity (MCP coderadar_compute_embeddings)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.compute_embeddings(model_name, batch_size),
        lambda metrics: render.compute_embeddings(
            metrics, search_similar="`coderadar search-similar`"))


@main.command(name="module-children")
@click.argument("module_id")
@_format_option
def module_children(module_id: str, fmt: str):
    """A module's contents (MCP coderadar_module_children)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.module_children(module_id), render.module_children)


@main.command(name="as-of")
@click.argument("timestamp")
@click.argument("symbols", nargs=-1)
@click.option("--query", "-q", "query_text", default="",
              help="Free text naming the symbols, instead of or besides SYMBOLS.")
@_format_option
def as_of(timestamp: str, symbols: tuple, query_text: str, fmt: str):
    """Look SYMBOLS up as of TIMESTAMP (MCP coderadar_as_of)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.as_of(timestamp, query_text, list(symbols)),
        lambda result: render.as_of(
            result,
            example=f'coderadar as-of --timestamp "{timestamp}" --symbols User',
            query="`coderadar query`", search="`coderadar search`"))


@main.command()
@click.argument("entity_id")
@click.option("--direction", default="both", show_default=True,
              type=click.Choice(["downstream", "upstream", "both", "out", "in"]),
              help="upstream = callers, importers, subclasses; downstream = callees, "
              "imports, bases (out/in are aliases).")
@click.option("--edge-kinds", "--edges", "edge_kinds", default=None,
              help="Comma-separated: calls, imports, extends (alias inherits), "
              "overrides. Default: all of them.")
@click.option("--max-depth", "--depth", "max_depth", default=3, show_default=True,
              help="At most 10.")
@_format_option
def traverse(entity_id: str, direction: str, edge_kinds: str | None, max_depth: int,
             fmt: str):
    """Walk any edge kind from ENTITY_ID (MCP coderadar_traverse)."""
    from . import ops, render
    _enter_project()
    kinds = [k.strip() for k in edge_kinds.split(",") if k.strip()] if edge_kinds else None
    _op(fmt, lambda: ops.traverse(entity_id, direction, kinds, max_depth), render.traverse)


# ── Analyses ─────────────────────────────────────────────────────────────

@main.command(name="get-smells")
@click.option("--entity-id", default=None, help="Only findings for this entity.")
@click.option("--rule-id", default=None, help="Only findings of this rule.")
@click.option("--strictness", default="normal", show_default=True,
              help="strict | normal | loose")
@_format_option
def get_smells(entity_id: str | None, rule_id: str | None, strictness: str, fmt: str):
    """Code-smell findings (MCP coderadar_get_smells)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.get_smells(entity_id, rule_id, strictness),
        lambda findings: render.get_smells(findings, entity_id, rule_id))


@main.command(name="dead-code")
@click.option("--min-confidence", default=0.6, show_default=True)
@click.option("--include-test-reachable", is_flag=True,
              help="Also report code that only tests reach.")
@click.option("--max-findings", default=100, show_default=True)
@_format_option
def dead_code(min_confidence: float, include_test_reachable: bool, max_findings: int,
              fmt: str):
    """Functions no entry point reaches — ranked evidence, not proof
    (MCP coderadar_dead_code)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.dead_code(min_confidence, include_test_reachable, max_findings),
        lambda findings: render.dead_code(findings, min_confidence))


@main.command(name="find-clones")
@click.option("--min-lines", default=10, show_default=True)
@click.option("--min-similarity", default=0.8, show_default=True)
@click.option("--max-groups", default=100, show_default=True)
@_format_option
def find_clones(min_lines: int, min_similarity: float, max_groups: int, fmt: str):
    """Duplicated code, token-level (MCP coderadar_find_clones)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.find_clones(min_lines, min_similarity, max_groups),
        lambda groups: render.find_clones(groups, min_lines, min_similarity))


@main.command(name="find-scaffolding")
@click.option("--include-secrets", is_flag=True, help="Also scan for hard-coded secrets.")
@click.option("--max-findings", default=100, show_default=True)
@_format_option
def find_scaffolding(include_secrets: bool, max_findings: int, fmt: str):
    """TODO markers, placeholder bodies, temp files (MCP coderadar_find_scaffolding)."""
    from . import ops, render
    _enter_project()
    _op(fmt, lambda: ops.find_scaffolding(include_secrets, max_findings),
        lambda findings: render.find_scaffolding(findings, include_secrets))


# ── Edits (dry run unless --apply) ───────────────────────────────────────

def _edit(fmt: str, call) -> None:
    from . import ops, render
    _ensure_graph()
    try:
        outcome = call()
    except ops.OpError as e:
        _fail(e)
    _emit(fmt, outcome, lambda o: render.edit(o, apply_hint=APPLY_HINT))


@main.command(name="replace-body")
@click.argument("entity_id")
@click.argument("new_body")
@click.option("--expected-hash", default=None,
              help="Refuse if the entity changed since you read it.")
@_apply_option
@_format_option
def replace_body(entity_id: str, new_body: str, expected_hash: str | None, apply: bool,
                 fmt: str):
    """Replace a function body; NEW_BODY '-' reads stdin (MCP coderadar_replace_body)."""
    from . import ops
    _enter_project()
    body = _text_arg(new_body)
    _edit(fmt, lambda: ops.replace_body(entity_id, body, expected_hash, not apply))


@main.command(name="update-signature")
@click.argument("entity_id")
@click.argument("new_signature")
@click.option("--inject-defaults", is_flag=True,
              help="Give new parameters defaults so call sites keep working.")
@_apply_option
@_format_option
def update_signature(entity_id: str, new_signature: str, inject_defaults: bool,
                     apply: bool, fmt: str):
    """Change a signature; call sites are listed for review
    (MCP coderadar_update_signature)."""
    from . import ops
    _enter_project()
    _edit(fmt, lambda: ops.update_signature(entity_id, new_signature, inject_defaults,
                                            not apply))


@main.command()
@click.argument("entity_id")
@click.argument("new_name")
@_apply_option
@_format_option
def rename(entity_id: str, new_name: str, apply: bool, fmt: str):
    """Rename an entity and every reference (MCP coderadar_rename)."""
    from . import ops
    _enter_project()
    _edit(fmt, lambda: ops.rename(entity_id, new_name, not apply))


@main.command(name="create-entity")
@click.argument("file_path")
@click.argument("language")
@click.argument("kind")
@click.argument("name")
@click.argument("body")
@click.option("--decorator", "decorators", multiple=True, help="Repeatable.")
@click.option("--anchor", default="end", show_default=True,
              help="end, top, or an entity id to insert after.")
@click.option("--signature", default=None, help="Full signature line (functions).")
@_apply_option
@_format_option
def create_entity(file_path: str, language: str, kind: str, name: str, body: str,
                  decorators: tuple, anchor: str, signature: str | None, apply: bool,
                  fmt: str):
    """Insert a function, class or constant into FILE_PATH; BODY '-' reads stdin
    (MCP coderadar_create_entity)."""
    from . import ops
    (file_path,) = _enter_project(file_path)
    text = _text_arg(body)
    _edit(fmt, lambda: ops.create_entity(file_path, language, kind, name, text,
                                         list(decorators) or None, anchor, signature,
                                         not apply))


# ── Index lifecycle ──────────────────────────────────────────────────────

@main.command()
@click.option("--full", is_flag=True,
              help="Walk the whole tree instead of re-parsing only changed files.")
@click.option("--exclude", "excludes", multiple=True,
              help="One-shot exclusion pattern (gitignore syntax), repeatable; "
              "merged with [project] exclude for this run only. Implies --full.")
@click.option("--with-embeddings", is_flag=True,
              help="Also compute embeddings for search-similar.")
@_format_option
def reindex(full: bool, excludes: tuple, with_embeddings: bool, fmt: str):
    """Bring the index up to date (MCP coderadar_reindex)."""
    _enter_project()
    _reindex(full, excludes, with_embeddings, fmt)


def _reindex(full: bool, excludes: tuple, with_embeddings: bool, fmt: str) -> None:
    from . import ops, render
    _activate(".")
    err_console.print(f"[dim]Indexing {Path.cwd()}...[/dim]")
    try:
        result = ops.reindex(with_embeddings, full, list(excludes) or None)
    except ops.OpError as e:
        _fail(e)
    _emit(fmt, result, render.reindex)


@main.command(name="update-file")
@click.argument("file_path", type=click.Path())
@click.option("--content", default=None,
              help="New content instead of reading the file ('-' reads stdin).")
@_format_option
def update_file(file_path: str, content: str | None, fmt: str):
    """Sync one file into the graph; a file deleted from disk is dropped
    (MCP coderadar_update_file)."""
    (file_path,) = _enter_project(file_path)
    _update_file(file_path, content, fmt)


def _update_file(file_path: str, content: str | None, fmt: str) -> None:
    from . import ops, render
    content = _text_arg(content)
    graph = _ensure_graph()
    try:
        report = ops.update_file(file_path, content, graph=graph)
    except ops.OpError as e:
        _fail(e)
    _emit(fmt, report, lambda r: render.update_file(file_path, r, content is not None))
    if not report.fully_applied:
        # A recovered parse is not a success a script may build on.
        raise SystemExit(1)


@main.command()
@_format_option
def status(fmt: str):
    """What is indexed here and how fresh it is (MCP coderadar_status).

    Loads the graph when the project has a store; never indexes.
    """
    _enter_project()
    _status(fmt, load=(Path(".coderadar") / "store").exists())


def _status(fmt: str, load: bool) -> None:
    from . import ops, render
    if load:
        _ensure_graph()
    try:
        result = ops.status()
    except ops.OpError as e:
        _fail(e)
    _emit(fmt, result, render.status)
    if fmt == "text":
        # The effective exclude list is visible, not implicit.
        _print_effective_excludes()


# ── Old names (hidden; they still work) ──────────────────────────────────

@main.command(hidden=True)
@click.argument("path", type=click.Path(exists=True, file_okay=False), default=".")
@click.option("--exclude", "excludes", multiple=True)
def analyze(path: str, excludes: tuple):
    """Old name of `coderadar -C PATH reindex --full`."""
    _renamed("analyze", "reindex --full")
    _enter_project(project=path, walk=False)
    _reindex(True, excludes, False, "text")


@main.command(hidden=True)
@click.argument("path", type=click.Path(exists=True, file_okay=False), default=".")
@click.option("--full", is_flag=True)
@click.option("--exclude", "excludes", multiple=True)
def rebuild(path: str, full: bool, excludes: tuple):
    """Old name of `coderadar -C PATH reindex --full`."""
    _renamed("rebuild", "reindex --full")
    _enter_project(project=path, walk=False)
    _reindex(True, excludes, False, "text")


@main.command(hidden=True)
@click.argument("file", type=click.Path())
@click.option("--content", default=None)
def update(file: str, content: str | None):
    """Old name of `coderadar update-file`."""
    _renamed("update", "update-file")
    (file,) = _enter_project(file)
    _update_file(file, content, "text")


@main.command(hidden=True)
def stats():
    """Old name of `coderadar status` (always loads the graph)."""
    _renamed("stats", "status")
    _enter_project()
    _status("text", load=True)


# ── Git ──────────────────────────────────────────────────────────────────

@main.group()
def git():
    """Git helpers: blame, is-clean, diff."""


@git.command(name="blame")
@click.argument("file", type=click.Path())
@click.option("--repo", default=".", help="Repository root")
def git_blame(file: str, repo: str):
    """Show git blame for a file (author per line)."""
    try:
        from coderadar._core import git_blame as _blame
        lines = _blame(repo, file)
    except ImportError:
        lines = []

    if not lines:
        console.print("[yellow]No blame data (git feature may be disabled)[/yellow]")
        return

    table = Table(title=f"Blame: {file}")
    table.add_column("Line", style="cyan")
    table.add_column("Author", style="green")
    table.add_column("Commit", style="dim")
    for l in lines:
        commit_short = l.get("commit", "")[:8]
        table.add_row(str(l.get("line", "")), l.get("author", ""), commit_short)
    console.print(table)


@git.command(name="is-clean")
@click.argument("repo", type=click.Path(exists=True), default=".")
def git_is_clean(repo: str):
    """Check if the git worktree is clean."""
    try:
        from coderadar._core import git_worktree_clean as _clean
        clean = _clean(repo).get("clean", True)
    except (ImportError, RuntimeError) as exc:
        # Defaulting to `clean = True` here reported a clean worktree for a
        # check that never ran — the answer a caller is most likely to act on.
        console.print(f"[red]Could not check the worktree:[/red] {exc}")
        raise SystemExit(1)

    if clean:
        console.print("[green]Worktree clean[/green]")
    else:
        console.print("[yellow]Worktree has uncommitted changes[/yellow]")


@git.command(name="diff")
@click.argument("repo", type=click.Path(exists=True), default=".")
@click.option("--old", "old_oid", default=None, help="Old commit OID")
@click.option("--new", "new_oid", default=None, help="New commit OID (default: HEAD)")
def git_diff(repo: str, old_oid: str | None, new_oid: str | None):
    """Show files changed between two commits."""
    try:
        from coderadar._core import git_changed_files as _diff
        files = _diff(repo, old_oid, new_oid)
    except ImportError:
        files = []
    except Exception as exc:  # noqa: BLE001 - R2-6: unknown revisions surface here, not as empty diff
        console.print(f"[red]Could not diff revisions:[/red] {exc}")
        raise SystemExit(1)

    if not files:
        console.print("[yellow]No changed files (or git feature disabled)[/yellow]")
        return

    console.print(f"[bold]{len(files)} changed files:[/bold]")
    for f in files:
        console.print(f"  {f}")


def _hidden_alias(command: click.Command, old: str, new: str) -> None:
    def callback(*args, **kwargs):
        _renamed(old, new)
        return command.callback(*args, **kwargs)
    main.add_command(click.Command(old, params=command.params, callback=callback,
                                   hidden=True, help=f"Old name of `coderadar {new}`."))


_hidden_alias(git_blame, "blame", "git blame")
_hidden_alias(git_is_clean, "git-clean", "git is-clean")
_hidden_alias(git_diff, "git-diff", "git diff")


@main.command()
@click.argument("entity_id")
def callers(entity_id: str):
    """List direct callers of ENTITY_ID (transitive: MCP coderadar_affected)."""
    _enter_project()
    from . import ops, render

    _ensure_graph()
    results = ops.callers(entity_id)

    if not results:
        # R2-16: typo'd ids reported the same "No callers" as truly
        # callerless entities. Pseudo-targets (external::) are addressable
        # graph members, not unknowns.
        if ops.find_entity(entity_id) is None and not entity_id.startswith("external::"):
            console.print(f"[yellow]Unknown entity: {entity_id}[/yellow]")
        else:
            console.print(f"[yellow]No callers found for {entity_id}[/yellow]")
        return

    console.print(render.callers(entity_id, results))


@main.command()
@click.argument("entity_id")
def callees(entity_id: str):
    """List direct callees of ENTITY_ID."""
    _enter_project()
    from . import ops, render

    _ensure_graph()
    results = ops.callees(entity_id)

    if not results:
        # R2-16: see callers() above.
        if ops.find_entity(entity_id) is None and not entity_id.startswith("external::"):
            console.print(f"[yellow]Unknown entity: {entity_id}[/yellow]")
        else:
            console.print(f"[yellow]No callees from {entity_id}[/yellow]")
        return

    console.print(render.callees(entity_id, results))


@main.command()
def shell():
    """REPL with persistent graph in memory."""
    _enter_project()
    console.print("[bold]CodeRadar Shell[/bold]")
    console.print("Type 'help' for commands, 'exit' to quit.")
    graph = _ensure_graph()

    while True:
        try:
            cmd = console.input("[bold cyan]>>[/bold cyan] ")
        except (EOFError, KeyboardInterrupt):
            break

        if cmd.strip() in ("exit", "quit"):
            break
        elif cmd.strip() == "help":
            console.print("Commands: query <pest>, traverse <id>, callers <id>, stats, exit")
        elif cmd.startswith("query "):
            query_str = cmd[6:]
            # R2-11: empty results printed nothing at all (looked hung).
            rows = list(graph.query(query_str))
            if not rows:
                console.print("[yellow]No results[/yellow]")
            for row in rows:
                console.print(row)
        elif cmd.startswith("traverse "):
            start_id = cmd[9:].strip()
            from .query import MacrameQuery
            rows = MacrameQuery(graph).traverse(start_id)
            if not rows:
                console.print("[yellow]No results[/yellow]")
            for row in rows:
                console.print(row)
        elif cmd.startswith("callers "):
            entity_id = cmd[8:].strip()
            from .query import MacrameQuery
            rows = MacrameQuery(graph).callers_of(entity_id)
            if not rows:
                console.print(f"[yellow]No callers found for {entity_id}[/yellow]")
            for row in rows:
                console.print(row)
        elif cmd.strip() == "stats":
            console.print(graph.stats())


@main.command()
@click.argument("snapshot", type=click.Path(exists=True))
@click.option("--root", default=None,
              help="Project root used with analyze (entity ids are path-keyed)")
def load_snapshot(snapshot: str, root: str | None):
    """Cold-start the in-memory graph from a Macrame ledger file."""
    import coderadar
    try:
        graph = coderadar.load(snapshot, root)
        stats = graph.stats()
        console.print(f"[green]Snapshot loaded: {stats}[/green]")
    except Exception as e:  # noqa: BLE001 - CLI boundary reports, then exits 1
        console.print(f"[yellow]load_snapshot failed:[/yellow] {e}")
        raise SystemExit(1)


@main.command(name="store-repair")
@click.option("--db", "db_path", default=None,
              help="Path to the Macrame store file (default: the project's).")
@click.option("--delete", is_flag=True,
              help="Delete the store file outright instead of repairing "
              "(last resort: unreadable rows cannot be retired, only dropped).")
def store_repair(db_path: str | None, delete: bool):
    """Report load-blocking rows and retire what is safely retireable.

    Prints live v1-leftover and unreadable counts, retires the v1 rows so
    the next cold load succeeds — an instant fix without reindexing.
    With --delete, removes the store file so the next analyze starts clean.
    """
    (db_path,) = _enter_project(db_path)
    db_path = db_path or ".coderadar/store/coderadar.db"
    from . import ops, render
    try:
        result = ops.store_repair(db_path, delete=delete)
    except Exception as e:  # noqa: BLE001 - CLI boundary reports, then exits 1
        console.print(f"[red]Repair failed:[/red] {e}")
        raise SystemExit(1)
    if "missing" in result:
        console.print(f"[yellow]{render.store_repair(db_path, result)}[/yellow]")
        return
    if "deleted" in result:
        console.print(f"[green]{render.store_repair(db_path, result)}[/green]")
        return
    console.print(render.store_repair(db_path, result))


def _print_effective_excludes() -> None:
    """Show the effective exclusion stack (item 7: visible, not implicit)."""
    try:
        from coderadar._core import default_excludes as _baseline
        baseline = list(_baseline())
    except ImportError:
        from .excludes import FALLBACK_BASELINE
        baseline = list(FALLBACK_BASELINE)
    user: list = []
    try:
        from .config import load_config
        user = list(load_config(Path(".")).project.exclude or [])
    except Exception:  # noqa: BLE001, S110 - best-effort: broken toml means baseline only
        pass
    # R1§6-13 follow-up: the .gitignore layer was invisible. Walk-level
    # passes honor it (WalkBuilder reads it under the exclude overrides),
    # so the stack shows its patterns; effect counts use the engine matcher
    # (baseline + config), which is what analyze/retraction enforce.
    gitignore: list = []
    try:
        for line in (Path(".") / ".gitignore").read_text(encoding="utf-8").splitlines():
            s = line.strip()
            if s and not s.startswith("#"):
                gitignore.append(s)
    except OSError:
        pass
    console.print("[bold]Effective excludes[/bold] (baseline + [project] exclude + .gitignore):")
    for pat in baseline:
        console.print(f"  [dim]baseline[/dim]  {pat}")
    for pat in user:
        console.print(f"  [cyan]config[/cyan]    {pat}")
    if gitignore:
        for pat in gitignore:
            console.print(f"  [yellow]gitignore[/yellow] {pat}")
    else:
        console.print("  [dim]gitignore  (no .gitignore)[/dim]")
    if not baseline and not user and not gitignore:
        console.print("  [dim](none)[/dim]")
    try:
        from .excludes import is_excluded as _is_excluded
        total = excluded = 0
        for dirpath, _dirnames, filenames in os.walk("."):
            for fn in filenames:
                total += 1
                if _is_excluded(os.path.join(dirpath, fn), "."):
                    excluded += 1
        console.print(f"[bold]Effect on .[/bold]: {total} file(s), "
                      f"{excluded} excluded, {total - excluded} indexed")
    except OSError as e:
        # os.walk ignores scandir errors itself; this is the walk root
        # going away mid-listing (or a broken symlink under is_excluded).
        console.print(f"[dim]Effect stats unavailable: {e}[/dim]")


@main.group()
def exclude():
    """Manage `[project] exclude` patterns in `.coderadar.toml`."""


@exclude.command(name="list")
def exclude_list():
    """Show user-configured excludes plus the built-in baseline."""
    _enter_project()
    _activate(".")
    _print_effective_excludes()


def _rewrite_excludes(path: Path, patterns: list) -> None:
    """Persist `patterns` as `[project] exclude` via text-level TOML edit.

    No TOML writer dependency: the existing file keeps every byte except
    the `exclude = [...]` line under `[project]` (added if absent). JSON
    string arrays are valid TOML string arrays, so `json.dumps` renders
    the value. The result is re-loaded to prove it still parses.
    """
    from .config import load_config
    value = json.dumps(sorted(set(patterns)))
    if path.exists():
        text = path.read_text(encoding="utf-8")
    else:
        text = ""
    lines = text.splitlines()
    in_project = False
    done = False
    out: list = []
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("["):
            if in_project and not done:
                out.append(f"exclude = {value}")
                done = True
            in_project = stripped == "[project]"
            out.append(line)
            continue
        if in_project and stripped.startswith("exclude") and "=" in stripped:
            out.append(f"exclude = {value}")
            done = True
            continue
        out.append(line)
    if not done:
        if not in_project:
            if out and out[-1].strip():
                out.append("")
            out.append("[project]")
        out.append(f"exclude = {value}")
    path.write_text("\n".join(out) + "\n", encoding="utf-8")
    load_config(path.parent)  # proves the edit still parses


@exclude.command(name="add")
@click.argument("pattern")
def exclude_add(pattern: str):
    """Add PATTERN to `[project] exclude` (takes effect on next analyze,
    which also retires the newly-excluded concepts from the store)."""
    _enter_project()
    from .config import load_config
    root = Path(".")
    current = list(load_config(root).project.exclude or [])
    if pattern in current:
        console.print(f"[dim]Already excluded:[/dim] {pattern}")
        return
    _rewrite_excludes(root / ".coderadar.toml", current + [pattern])
    console.print(f"[green]OK[/green]  Excluded {pattern} — re-analyze to apply.")


@exclude.command(name="remove")
@click.argument("pattern")
def exclude_remove(pattern: str):
    """Remove PATTERN from `[project] exclude`."""
    _enter_project()
    from .config import load_config
    root = Path(".")
    current = list(load_config(root).project.exclude or [])
    if pattern not in current:
        console.print(f"[yellow]Not excluded:[/yellow] {pattern}")
        return
    _rewrite_excludes(
        root / ".coderadar.toml", [p for p in current if p != pattern]
    )
    console.print(f"[green]OK[/green]  No longer excluded: {pattern}.")


@main.command()
@click.argument("viz_type")
@click.argument("args", nargs=-1)
@click.option("--output", "-o", type=click.Path())
@click.option("--format", "fmt", default="mermaid")
def visualize(viz_type: str, args: tuple, output: str | None, fmt: str):
    """Run a visualizer: hierarchy, dependencies, call-graph."""
    (output,) = _enter_project(output)
    from .visualizers import NothingToVisualize
    from .visualizers.call_graph import generate_call_graph
    from .visualizers.graphviz_viz import generate_dot
    from .visualizers.mermaid import generate_mermaid

    graph = _ensure_graph()
    arg_list = list(args)

    # R2-14: anything but the supported formats used to render mermaid
    # anyway, silently.
    if fmt not in ("mermaid", "graphviz", "dot"):
        console.print(f"[red]Unknown format:[/red] {fmt} "
                      f"(supported: mermaid, graphviz, dot)")
        raise SystemExit(1)

    try:
        text = _render(viz_type, fmt, arg_list, graph,
                       generate_dot, generate_mermaid, generate_call_graph)
    except NothingToVisualize as exc:
        # Exiting 0 here is how a fabricated diagram used to reach the user.
        # R2-16: for call-graph with an id-form arg, distinguish an unknown
        # id from a known-but-edgeless entity (name-form args resolve via
        # search upstream, so only id-form args are checked here).
        if viz_type == "call-graph" and arg_list and "::" in str(arg_list[0]):
            from .query import MacrameQuery
            if MacrameQuery(graph).find(str(arg_list[0])) is None:
                console.print(f"[red]Unknown entity:[/red] {arg_list[0]}")
                raise SystemExit(1)
        console.print(f"[red]Nothing to visualize:[/red] {exc}")
        raise SystemExit(1)

    if text is None:
        console.print(f"[red]Unknown visualization type: {viz_type}[/red]")
        raise SystemExit(1)

    if output:
        Path(output).write_text(text, encoding="utf-8")
        console.print(f"[green]Written to {output}[/green]")
    else:
        console.print(text)


def _render(viz_type, fmt, arg_list, graph,
            generate_dot, generate_mermaid, generate_call_graph):
    """Pick a renderer. Returns None for an unknown type."""
    if viz_type == "hierarchy":
        if fmt in ("graphviz", "dot"):
            text = generate_dot("hierarchy", arg_list, graph)
        else:
            text = generate_mermaid("hierarchy", arg_list, graph)
    elif viz_type == "dependencies":
        if fmt in ("graphviz", "dot"):
            text = generate_dot("dependencies", arg_list, graph)
        else:
            text = generate_mermaid("dependencies", arg_list, graph)
    elif viz_type == "call-graph":
        # --format is honoured here too; call_graph.py renders Mermaid only,
        # so graphviz goes through the dot renderer like the other two.
        if fmt in ("graphviz", "dot"):
            text = generate_dot("call-graph", arg_list, graph)
        else:
            text = generate_call_graph(arg_list, graph)
    else:
        return None
    return text


@main.command()
@click.option("--unresolved", is_flag=True, help="Show all unresolved references")
@click.option("--low-confidence", is_flag=True, help="List edges below min_confidence")
def diagnose(unresolved: bool, low_confidence: bool):
    """Show unresolved references or ambiguous edges."""
    _enter_project()
    # Both flags used to print a header and no rows, which reads as a clean
    # bill of health rather than a report that was never written.
    from . import ops, render

    if not unresolved and not low_confidence:
        unresolved = low_confidence = True

    _ensure_graph()
    console.print(render.diagnose(
        ops.diagnose(unresolved=unresolved, low_confidence=low_confidence)))


@main.group()
def mcp():
    """Model Context Protocol server commands."""


@mcp.command()
@click.option("--path", "project_path", type=click.Path(exists=True), default=None,
              help="Project root to serve. Defaults to walking up from the cwd "
                   "looking for a .coderadar marker.")
def serve(project_path: str | None):
    """Start the CodeRadar MCP server over stdio.

    Connect an MCP client (Claude Code, Cursor, etc.) to get the
    coderadar_* tools over the indexed project: explore,
    search, structural and temporal queries, smells, dead code, clones, and
    the dry-run/apply mutation pipeline. The server's instructions tell the
    agent how to use them.

    With no --path, the project root is found by walking up from the cwd
    looking for a .coderadar marker, and the client's declared roots are
    consulted on the first tool call.

    Configure your MCP client with:
      {
        "mcpServers": {
          "coderadar": {
            "command": "uv",
            "args": ["run", "coderadar", "mcp", "serve"]
          }
        }
      }
    """
    import coderadar

    from .mcp import serve as mcp_serve
    from .mcp.lazy import LazyRootRetry
    from .mcp.lazy import configure as lazy_configure
    from .mcp.roots import adopt_project_root, describe, resolve_project_root
    from .mcp.startup import BackgroundIndex, configure

    # Capture where the client launched us BEFORE anything chdirs: this is
    # the launch directory, and it keys the last-project record (P2-3) —
    # the previous-session rung below, and the set_project rewrite later.
    launch_cwd = Path(os.getcwd())

    # MCP clients launch servers from wherever they happen to be, so the cwd
    # is a poor guess and `--path` is optional. Climb the ladder, then move
    # the process onto the answer: every read helper in the server resolves
    # graph paths against the cwd, and entity ids carry the prefix analyze()
    # walked, so cwd and root have to be the same directory or lookups miss.
    resolved = resolve_project_root(path_flag=project_path, launch_cwd=launch_cwd)
    adopt_project_root(resolved)
    print(describe(resolved), file=sys.stderr)

    # stdout is the JSON-RPC transport from here on; anything printed to it
    # that is not a protocol frame desynchronises the client. _activate
    # reports ignored config keys through rich, which writes to stdout.
    import contextlib
    with contextlib.redirect_stdout(sys.stderr):
        _activate(".")

    # Index on a background thread so the client's `initialize` is answered
    # at once. Indexing a large repo takes minutes, and a client that waits
    # that long for the handshake concludes the server is hung rather than
    # starting. Every tool handler calls `ensure_ready()` on its way in, so a
    # call that arrives early waits for the index and then reports progress
    # instead of answering from a half-built graph.
    #
    # This is only safe because `analyze` releases the GIL; around a
    # GIL-holding analyze, the background thread would have frozen the event
    # loop for the whole index.
    #
    # '.' rather than the absolute root on purpose: entity ids are prefixed
    # with the path walked, and `_reindex` re-walks '.' later. Both spellings
    # have to agree or the second index orphans the first one's ids.
    index = BackgroundIndex(root=".")
    configure(index)
    index.start()

    # If nothing on disk confirmed the root, the first tool call asks the
    # client where its workspace is and re-indexes if the answer is better.
    lazy_configure(LazyRootRetry(resolved, index, path_flag=project_path))
    print(f"Indexing {resolved.path} in the background...", file=sys.stderr)

    # set_project rewrites the launch-directory record (P2-3); it needs the
    # same pre-chdir directory captured above.
    from .mcp.server import set_launch_cwd
    set_launch_cwd(launch_cwd)
    mcp_serve(coderadar.CodeGraph())


@main.command()
@click.argument("paths", nargs=-1, type=click.Path(exists=True))
@click.option("--debounce", default=100, help="Debounce window in ms")
def watch(paths, debounce):
    """Watch files for changes and auto-update the code graph.

    PATHS: directories to watch (default: src/ tests/).
    """
    paths = tuple(_enter_project(*paths))
    watch_paths = list(paths) if paths else ["src/", "tests/"]
    # This is the second of two commands that were both named `watch`; the
    # first was dead (click registers by function name, so this one won) and
    # took the config activation with it. A watcher updating an empty index
    # reports changes against nothing.
    graph = _ensure_graph()
    watcher = graph.watch(watch_paths, debounce_ms=debounce)
    console.print(f"[bold green]Watching:[/bold green] {', '.join(watch_paths)}")
    console.print("[dim]Press Ctrl+C to stop[/dim]")
    watcher.run_forever()


if __name__ == "__main__":
    main()
