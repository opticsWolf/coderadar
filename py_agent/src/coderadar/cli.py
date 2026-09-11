"""CodeRadar v3.6 — Command-Line Interface (§16)"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Optional

import click
from rich.console import Console
from rich.table import Table

from . import __version__

console = Console()
# R2-15/R2-13: diagnostics (cold-start notes, fallbacks, config nudges)
# go to stderr, so stdout stays machine-readable (pipes, --format json).
err_console = Console(file=sys.stderr)


def _run_framework_extraction(project_root: Path) -> dict:
    """v3.6: Run framework resolvers on a project and return summary.

    Detects Django/Flask/FastAPI projects and extracts route nodes
    and handler edges. Synthetic edges are registered in the Rust
    graph so agents can trace them via callers_of / callees_of.
    """
    from coderadar.resolvers import ALL_RESOLVERS

    results = {"routes": 0, "handlers": 0, "frameworks": [], "edges_registered": 0}
    # Collected across every resolver and file, then registered in one call:
    # the per-edge variant clones the whole ProjectedGraph each time.
    synthetic_edges: list[tuple[str, str, str]] = []
    for resolver_cls in ALL_RESOLVERS:
        resolver = resolver_cls()
        if not resolver.detect(project_root):
            continue
        results["frameworks"].append(resolver.name)
        for py_file in project_root.rglob("*.py"):
            if py_file.name.startswith("__"):
                continue
            try:
                source = py_file.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            extraction = resolver.extract(str(py_file), source)
            results["routes"] += len(extraction.nodes)
            results["handlers"] += len(extraction.edges)
            synthetic_edges.extend(
                (edge.source_id, edge.target_id, edge.kind.upper())
                for edge in extraction.edges
            )

    if synthetic_edges:
        try:
            from coderadar._core import register_synthetic_edges_bulk
            report = register_synthetic_edges_bulk(synthetic_edges)
            results["edges_registered"] = int(report.get("registered", 0))
        except (ImportError, RuntimeError):
            # Graph not loaded or _core not available — edges displayed only
            pass
    return results


def _extract_star_exports(project_root: Path) -> int:
    """v0.5: Extract __all__ exports from Python modules.

    Scans all .py files, statically detects __all__ lists, and
    registers them via set_module_star_exports so wildcard
    imports (from X import *) can be resolved.
    """
    from coderadar.resolvers.exports import extract_all_exports
    try:
        from coderadar._core import set_module_star_exports_bulk
    except ImportError:
        return 0

    # Collected, then applied in one call — the per-module variant clones the
    # whole ProjectedGraph each time.
    entries: list[tuple[str, list[str]]] = []
    for py_file in project_root.rglob("*.py"):
        try:
            source = py_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        names = extract_all_exports(source)
        if names:
            entries.append((f"{py_file}::module", names))
    if not entries:
        return 0
    try:
        report = set_module_star_exports_bulk(entries)
    except RuntimeError:
        return 0
    return int(report.get("applied", 0))


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
    except Exception as exc:  # a broken config must not kill the command
        console.print(f"[yellow]Config not applied:[/yellow] {exc}")
        return
    if activated.ignored:
        console.print(
            f"[dim]Config: {len(activated.ignored)} setting(s) with no consumer "
            f"were ignored ({', '.join(activated.ignored[:3])}"
            f"{', ...' if len(activated.ignored) > 3 else ''})[/dim]"
        )


# v0.8 P2-4: the staleness rules live in coderadar.coldstart — the single
# implementation shared by the CLI's load/analyze ordering and the MCP
# background index's incremental cold start.
from .coldstart import (
    INDEXABLE_EXTS as _INDEXABLE_EXTS,
    STALENESS_SKIP_DIRS as _STALENESS_SKIP_DIRS,
    store_db_path as _store_db_path,
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
        except Exception as exc:
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
def main():
    """CodeRadar — live semantic graph of your codebase.

    Maintains an incrementally updatable graph of code structure,
    enabling LLMs and developers to query, visualize, and safely rewrite code.
    """


@main.command()
@click.argument("path", type=click.Path(exists=True), default=".")
@click.option("--force", is_flag=True, help="Overwrite an existing .coderadar.toml")
def init(path: str, force: bool):
    """Initialize CodeRadar in a project directory.

    Writes .coderadar.toml, creates .coderadar/store/, and runs the first
    analysis.
    """
    from pathlib import Path
    from datetime import datetime

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
# Generated by `coderadar init` on {datetime.now().isoformat()}
#
# `coderadar analyze` prints a line naming any key it could not use, so a
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
    console.print(f"\n[bold]Running initial analysis...[/bold]")
    import coderadar
    graph = coderadar.analyze(str(root), create_store=True)
    stats = graph.stats()
    console.print(f"  Modules:    {stats.get('modules', 0)}")
    console.print(f"  Classes:    {stats.get('classes', 0)}")
    console.print(f"  Functions:  {stats.get('functions', 0)}")
    console.print(f"  Imports:    {stats.get('imports', 0)}")
    console.print(f"  Call edges: {stats.get('call_edges', 0)}")
    # v3.6: Run framework resolvers for Django/Flask/FastAPI
    framework = _run_framework_extraction(root)
    if framework["frameworks"]:
        console.print(f"  Frameworks: {', '.join(framework['frameworks'])}")
        console.print(f"  Routes:     {framework['routes']}")
        console.print(f"  Handlers:   {framework['handlers']}")
    # v0.5: Extract __all__ star exports for wildcard import resolution
    star_count = _extract_star_exports(root)
    if star_count > 0:
        console.print(f"  Star exports: {star_count} module(s) with __all__")
    console.print(f"[green]OK  Analysis complete[/green]")


@main.command()
@click.argument("path", type=click.Path(exists=True), default=".")
@click.option("--exclude", "excludes", multiple=True,
              help="One-shot exclusion pattern (gitignore syntax), repeatable. "
              "Merged with [project] exclude + baseline for this run only.")
def analyze(path: str, excludes: tuple):
    """One-shot analysis without persistence."""
    _activate(path)
    console.print(f"[bold]Analyzing[/bold] {path}...")
    import coderadar
    graph = coderadar.analyze(path, exclude=list(excludes) or None)
    stats = graph.stats()
    table = Table(title="Analysis Results")
    table.add_column("Kind", style="cyan")
    table.add_column("Count", style="green")
    for kind, count in stats.items():
        table.add_row(kind, str(count))
    console.print(table)
    # v3.6: Run framework resolvers
    framework = _run_framework_extraction(Path(path))
    if framework["frameworks"]:
        fw_table = Table(title="Framework Extraction")
        fw_table.add_column("Framework", style="cyan")
        fw_table.add_column("Routes", style="green")
        fw_table.add_column("Handler Edges", style="yellow")
        fw_table.add_row(
            ", ".join(framework["frameworks"]),
            str(framework["routes"]),
            str(framework["handlers"]),
        )
        console.print(fw_table)
    # v0.5: Extract __all__ star exports for wildcard import resolution
    star_count = _extract_star_exports(Path(path))
    if star_count > 0:
        console.print(f"[dim]  Star exports: {star_count} module(s) with __all__[/dim]")
    console.print(f"[green]OK  Analysis complete[/green]")


@main.command()
@click.argument("file", type=click.Path(exists=True))
@click.option("--content", default=None, help="New content (use '-' for stdin)")
def update(file: str, content: Optional[str]):
    """One-shot update for a single file."""
    if content == "-":
        content = sys.stdin.read()

    graph = _ensure_graph()
    report = graph.update_file(file, content)
    console.print(f"  Fully applied: {report.fully_applied}")
    console.print(f"  Parse quality: {report.parse_quality}")
    console.print(f"  Parse errors:  {report.parse_errors}")
    console.print(f"  Elapsed:       {report.elapsed_ms:.1f}ms")
    if not report.fully_applied:
        # This printed "Fully applied: False" and exited 0, so a script
        # driving updates could not tell a failure from a success.
        raise SystemExit(1)


@main.command()
@click.argument("query_string")
@click.option("--format", "fmt", type=click.Choice(["table", "json"]),
              default="table", help="Output format (json for machine readers).")
def query(query_string: str, fmt: str):
    """Execute a Pest query; pretty-print results."""
    graph = _ensure_graph()
    results = list(graph.query(query_string))

    if not results:
        console.print("[yellow]No results[/yellow]")
        return

    if fmt == "json":
        # R2-15: machine mode -- agents are the primary readers; a table
        # shaped for a tty is data loss through a pipe.
        import json as _json
        # soft_wrap: rich must not fold long lines mid-string (that would
        # corrupt the JSON for parsers); piped output goes through verbatim.
        console.print(_json.dumps(results, indent=2, default=str),
                        soft_wrap=True)
        return

    # Build table from keys of first result. R2-15: when piped, rich
    # squeezed every column to a few chars (`com�`). A pipe has no width,
    # so render wide instead -- identity columns never wrap (grep-able),
    # the rest folds only past a generous width. Interactive terminals
    # keep auto-detected width (this branch is pipe-only).
    if sys.stdout.isatty():
        tbl_console = console
    else:
        from rich.console import Console as _Console
        tbl_console = _Console(file=sys.stdout, width=250)
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


@main.command()
@click.argument("start_id")
@click.option("--depth", default=3, help="Maximum traversal depth")
@click.option("--edges", default="CALLS", help="Edge types (comma-separated)")
@click.option("--direction", default="both",
              type=click.Choice(["in", "out", "both"]))
def traverse(start_id: str, depth: int, edges: str, direction: str):
    """Traverse the graph from start_id via Macrame."""
    from .query import MacrameQuery

    graph = _ensure_graph()
    mq = MacrameQuery(graph)
    edge_types = [e.strip() for e in edges.split(",")] if edges else None
    try:
        results = mq.traverse(start_id, depth, edge_types, direction)
    except ValueError as exc:
        # R2-9: unknown edge kinds surface here (not as "No results").
        console.print(f"[red]Traversal error:[/red] {exc}")
        raise SystemExit(1)

    if not results:
        # R2-16: an unknown start id and a known-but-isolated one used to
        # report the identical "No results".
        if mq.find(start_id) is None and not start_id.startswith("external::"):
            console.print(f"[yellow]Unknown entity: {start_id}[/yellow]")
        else:
            console.print("[yellow]No results[/yellow]")
        return

    table = Table(title=f"Traversal from [bold]{start_id}[/bold]")
    for key in results[0]:
        table.add_column(key, style="cyan")
    for row in results:
        table.add_row(*[str(row.get(k, "")) for k in results[0]])
    console.print(table)
    console.print(f"[dim]{len(results)} reached[/dim]")


@main.command()
@click.argument("entity_id")
def callers(entity_id: str):
    """List callers of an entity."""
    from .query import MacrameQuery

    graph = _ensure_graph()
    mq = MacrameQuery(graph)
    results = mq.callers_of(entity_id)

    if not results:
        # R2-16: typo'd ids reported the same "No callers" as truly
        # callerless entities. Pseudo-targets (external::) are addressable
        # graph members, not unknowns.
        if mq.find(entity_id) is None and not entity_id.startswith("external::"):
            console.print(f"[yellow]Unknown entity: {entity_id}[/yellow]")
        else:
            console.print(f"[yellow]No callers found for {entity_id}[/yellow]")
        return

    console.print(f"[bold]Callers of {entity_id}:[/bold]")
    for r in results:
        _rid = str(r.get('id', r.get('entity_id', '?')))
        if _rid.startswith('external::'):
            # R2-1: external/builtin targets have no file/line -- say so.
            console.print(f"  {_rid} (external)")
            continue
        _f = r.get('file', None) or r.get('file_path', None) or r.get('path', None)
        if not _f and '::' in _rid:
            _f = _rid.split('::')[0]
        console.print(f"  {_rid} ({_f or '?'}:{r.get('line', '?')})")


@main.command()
@click.argument("entity_id")
def callees(entity_id: str):
    """List callees called by an entity."""
    from .query import MacrameQuery

    graph = _ensure_graph()
    mq = MacrameQuery(graph)
    results = mq.callees_of(entity_id)

    if not results:
        # R2-16: see callers() above.
        if mq.find(entity_id) is None and not entity_id.startswith("external::"):
            console.print(f"[yellow]Unknown entity: {entity_id}[/yellow]")
        else:
            console.print(f"[yellow]No callees from {entity_id}[/yellow]")
        return

    console.print(f"[bold]Callees from {entity_id}:[/bold]")
    for r in results:
        _rid = str(r.get('id', r.get('entity_id', '?')))
        if _rid.startswith('external::'):
            # R2-1: external/builtin targets have no file/line -- say so.
            console.print(f"  {_rid} (external)")
            continue
        _f = r.get('file', None) or r.get('file_path', None) or r.get('path', None)
        if not _f and '::' in _rid:
            _f = _rid.split('::')[0]
        console.print(f"  {_rid} ({_f or '?'}:{r.get('line', '?')})")


@main.command()
def shell():
    """REPL with persistent graph in memory."""
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
def load_snapshot(snapshot: str, root: Optional[str]):
    """Cold-start the in-memory graph from a Macrame ledger file (v0.8 P1)."""
    import coderadar
    try:
        graph = coderadar.load(snapshot, root)
        stats = graph.stats()
        console.print(f"[green]Snapshot loaded: {stats}[/green]")
    except Exception as e:
        console.print(f"[yellow]load_snapshot failed:[/yellow] {e}")
        raise SystemExit(1)


@main.command()
@click.argument("path", type=click.Path(exists=True), default=".")
@click.option("--full", is_flag=True, help="Accepted for compatibility; "
              "a rebuild is always a full re-index")
@click.option("--exclude", "excludes", multiple=True,
              help="One-shot exclusion pattern (gitignore syntax), repeatable.")
def rebuild(path: str, full: bool, excludes: tuple):
    """Re-index the project from scratch.

    This printed "Rebuilding..." and returned — a command that reported
    success for work it never started.
    """
    import coderadar

    _activate(path)
    console.print(f"[bold]Rebuilding[/bold] {path}...")
    graph = coderadar.analyze(path, exclude=list(excludes) or None)
    stats = graph.stats()
    console.print(
        f"[green]OK[/green]  {stats.get('file_count', 0)} file(s), "
        f"{stats.get('functions', 0)} function(s), "
        f"{stats.get('classes', 0)} class(es), "
        f"{stats.get('call_edges', 0)} call edge(s)"
    )


@main.command(name="store-repair")
@click.option("--db", "db_path", default=".coderadar/store/coderadar.db",
              help="Path to the Macrame store file.")
@click.option("--delete", is_flag=True,
              help="Delete the store file outright instead of repairing "
              "(last resort: unreadable rows cannot be retired, only dropped).")
def store_repair(db_path: str, delete: bool):
    """Report load-blocking rows and retire what is safely retireable (F6).

    Prints live v1-leftover and unreadable counts, retires the v1 rows so
    the next cold load succeeds — an instant fix without reindexing.
    With --delete, removes the store file so the next analyze starts clean.
    """
    from pathlib import Path
    p = Path(db_path)
    if not p.exists():
        console.print(f"[yellow]No store at {p} — nothing to repair.[/yellow]")
        return
    if delete:
        p.unlink()
        console.print(f"[green]OK[/green]  Deleted {p}; next analyze rebuilds from scratch.")
        return
    try:
        from coderadar._core import store_repair as _repair
        rep = _repair(str(p))
    except Exception as e:
        console.print(f"[red]Repair failed:[/red] {e}")
        raise SystemExit(1)
    console.print(f"[bold]Store repair: {p}[/bold]")
    console.print(f"  v1 leftovers found:   {rep.get('v1_found', 0)}")
    console.print(f"  v1 leftovers retired: {rep.get('v1_retired', 0)}")
    console.print(f"  edges retired:        {rep.get('edges_retired', 0)}")
    console.print(f"  unreadable rows:      {rep.get('unreadable_live', 0)}")
    if rep.get('unreadable_live', 0):
        console.print("[yellow]Unreadable rows cannot be retired — if cold load still "
                        "fails, re-run with --delete.[/yellow]")
    elif not rep.get('v1_found', 0):
        console.print("[green]OK[/green]  Store is clean — nothing to retire.")
    else:
        console.print("[green]OK[/green]  Next cold load should succeed.")


@main.command()
def stats():
    """Counts, parse-error summary, memory usage."""
    graph = _ensure_graph()
    s = graph.stats()
    table = Table(title="Graph Statistics")
    table.add_column("Metric", style="cyan")
    table.add_column("Value", style="green")
    for k, v in s.items():
        table.add_row(k, str(v))
    console.print(table)
    # Item 7: the effective exclude list is visible, not implicit.
    _print_effective_excludes()


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
    except Exception:
        pass
    console.print("[bold]Effective excludes[/bold] (baseline + [project] exclude):")
    for pat in baseline:
        console.print(f"  [dim]baseline[/dim]  {pat}")
    for pat in user:
        console.print(f"  [cyan]config[/cyan]    {pat}")
    if not baseline and not user:
        console.print("  [dim](none)[/dim]")


@main.group()
def exclude():
    """Manage `[project] exclude` patterns in `.coderadar.toml`."""


@exclude.command(name="list")
def exclude_list():
    """Show user-configured excludes plus the built-in baseline."""
    _activate(".")
    _print_effective_excludes()


def _rewrite_excludes(path: Path, patterns: list) -> None:
    """Persist `patterns` as `[project] exclude` via text-level TOML edit.

    No TOML writer dependency: the existing file keeps every byte except
    the `exclude = [...]` line under `[project]` (added if absent). JSON
    string arrays are valid TOML string arrays, so `json.dumps` renders
    the value. The result is re-loaded to prove it still parses.
    """
    import json
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
def visualize(viz_type: str, args: tuple, output: Optional[str], fmt: str):
    """Run a visualizer: hierarchy, dependencies, call-graph."""
    from .visualizers import NothingToVisualize
    from .visualizers.mermaid import generate_mermaid
    from .visualizers.graphviz_viz import generate_dot
    from .visualizers.call_graph import generate_call_graph

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
    """Show unresolved references or ambiguous edges.

    Both flags used to print a header and no rows, which reads as a clean
    bill of health rather than a report that was never written.
    """
    from coderadar._core import index_edge_stats, search_entities, traverse_unresolved

    if not unresolved and not low_confidence:
        unresolved = low_confidence = True

    _ensure_graph()

    if unresolved:
        from coderadar._core import unresolved_targets as _unresolved_names
        console.print("[bold]Unresolved references:[/bold]")
        rows = []
        for fn in search_entities("", 1000, "function"):
            # R2-12: attribute each function's OWN targets (the old
            # traverse_unresolved count summed the 1-hop neighborhood,
            # which listed callers for their callees' gaps).
            names = ", ".join(_unresolved_names(fn["id"]))
            if names:
                rows.append((fn["id"], names))
        if not rows:
            console.print("  [green]none[/green]")
        else:
            table = Table()
            table.add_column("Entity", style="cyan")
            table.add_column("Unresolved call targets", style="yellow")
            for entity_id, names in sorted(rows, key=lambda r: -len(r[1])):
                table.add_row(entity_id, names)
            console.print(table)
            console.print(
                f"[dim]{len(rows)} function(s) call targets the graph "
                f"cannot follow[/dim]"
            )

    if low_confidence:
        console.print("[bold]Ambiguous base classes:[/bold]")
        stats = index_edge_stats()
        details = stats.get("ambiguous_base_details") or []
        if not details:
            console.print("  [green]none[/green]")
        else:
            for detail in details:
                console.print(f"  {detail}")
            console.print(f"[dim]{stats.get('ambiguous_bases', 0)} ambiguous[/dim]")


@main.command()
def status():
    """Report what is indexed here, and how stale it is.

    This printed "CodeRadar is running" unconditionally — a health check
    that could not fail, and that said nothing about the project it was run
    in.
    """
    import time
    from pathlib import Path
    from coderadar._core import graph_stats

    root = Path.cwd()
    config = root / ".coderadar.toml"
    store = root / ".coderadar" / "store"
    console.print(f"[bold]Project:[/bold] {root}")
    console.print(f"  Config: {config if config.exists() else '[yellow]none[/yellow]'}")
    console.print(f"  Store:  {store if store.exists() else '[yellow]none[/yellow]'}")

    try:
        stats = graph_stats()
    except RuntimeError:
        # Every command runs in its own process, so this is the normal state
        # outside a server — not a fault.
        console.print("  Index:  [yellow]not loaded in this process[/yellow]")
        if not store.exists():
            console.print("  [dim]Run `coderadar init` to create one.[/dim]")
        return

    indexed_at = stats.get("indexed_at")
    age = ""
    if indexed_at:
        seconds = max(0.0, time.time() - float(indexed_at))
        age = f" ({seconds / 60:.0f} min ago)" if seconds >= 60 else " (just now)"
    console.print(
        f"  Index:  [green]loaded[/green]{age} — "
        f"{stats.get('file_count', 0)} file(s), "
        f"{stats.get('functions', 0)} function(s), "
        f"{stats.get('classes', 0)} class(es)"
    )


@main.group()
def mcp():
    """Model Context Protocol server commands."""


@mcp.command()
@click.option("--path", "project_path", type=click.Path(exists=True), default=None,
              help="Project root to serve. Defaults to walking up from the cwd "
                   "looking for a .coderadar marker.")
def serve(project_path: str | None):
    """Start the CodeRadar MCP server over stdio.

    Connect an MCP client (Claude Code, Cursor, etc.) to this server to get
    code intelligence over the indexed project — 19 tools covering search,
    exploration, structural and temporal queries, code smells, and the
    plan/review/apply mutation pipeline.

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
    from .mcp.roots import adopt_project_root, describe, resolve_project_root
    from .mcp.startup import BackgroundIndex, configure
    from .mcp.lazy import LazyRootRetry, configure as lazy_configure

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
@click.argument("file", type=click.Path())
@click.option("--repo", default=".", help="Repository root")
def blame(file: str, repo: str):
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


@main.command()
@click.argument("repo", type=click.Path(exists=True), default=".")
def git_clean(repo: str):
    """Check if git worktree is clean."""
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


@main.command()
@click.argument("repo", type=click.Path(exists=True), default=".")
@click.option("--old", "old_oid", default=None, help="Old commit OID")
@click.option("--new", "new_oid", default=None, help="New commit OID (default: HEAD)")
def git_diff(repo: str, old_oid: Optional[str], new_oid: Optional[str]):
    """Show files changed between two commits."""
    try:
        from coderadar._core import git_changed_files as _diff
        files = _diff(repo, old_oid, new_oid)
    except ImportError:
        files = []
    except Exception as exc:
        # R2-6: unknown revisions surface here (not as an empty diff).
        console.print(f"[red]Could not diff revisions:[/red] {exc}")
        raise SystemExit(1)

    if not files:
        console.print("[yellow]No changed files (or git feature disabled)[/yellow]")
        return

    console.print(f"[bold]{len(files)} changed files:[/bold]")
    for f in files:
        console.print(f"  {f}")


@main.command()
@click.argument("paths", nargs=-1, type=click.Path(exists=True))
@click.option("--debounce", default=100, help="Debounce window in ms")
def watch(paths, debounce):
    """Watch files for changes and auto-update the code graph.

    PATHS: directories to watch (default: src/ tests/).
    """
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
