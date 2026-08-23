"""CodeRadar v3.6 — Command-Line Interface (§16)"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Optional

import click
from rich.console import Console
from rich.table import Table

console = Console()


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


@click.group()
@click.version_option(version="0.6.32", prog_name="coderadar",
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
    graph = coderadar.analyze(str(root))
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
def analyze(path: str):
    """One-shot analysis without persistence."""
    _activate(path)
    console.print(f"[bold]Analyzing[/bold] {path}...")
    import coderadar
    graph = coderadar.analyze(path)
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

    import coderadar
    graph = coderadar.CodeGraph()
    report = graph.update_file(file, content)
    console.print(f"  Fully applied: {report.fully_applied}")
    console.print(f"  Parse quality: {report.parse_quality}")
    console.print(f"  Parse errors:  {report.parse_errors}")
    console.print(f"  Elapsed:       {report.elapsed_ms:.1f}ms")


@main.command()
@click.argument("path", type=click.Path(exists=True), default=".")
def watch(path: str):
    """Long-running watcher; JSONL on stdout."""
    _activate(path)
    import coderadar
    console.print(f"[bold]Watching[/bold] {path}...")
    with coderadar.watch(path) as w:
        for report in w:
            entry = {
                "affected_files": report.affected_files,
                "parse_quality": report.parse_quality,
                "fully_applied": report.fully_applied,
                "elapsed_ms": report.elapsed_ms,
            }
            console.print_json(data=entry)


@main.command()
@click.argument("query_string")
def query(query_string: str):
    """Execute a Pest query; pretty-print results."""
    import coderadar
    graph = coderadar.CodeGraph()
    results = list(graph.query(query_string))

    if not results:
        console.print("[yellow]No results[/yellow]")
        return

    # Build table from keys of first result
    table = Table(title=f"Query: {query_string}")
    for key in results[0]:
        table.add_column(key, style="cyan")
    for row in results:
        table.add_row(*[str(row.get(k, "")) for k in results[0]])
    console.print(table)
    console.print(f"[dim]{len(results)} result(s)[/dim]")


@main.command()
@click.argument("start_id")
@click.option("--depth", default=3, help="Maximum traversal depth")
@click.option("--edges", default="CALLS", help="Edge types (comma-separated)")
@click.option("--direction", default="both",
              type=click.Choice(["in", "out", "both"]))
def traverse(start_id: str, depth: int, edges: str, direction: str):
    """Traverse the graph from start_id via Macrame."""
    from .query import MacrameQuery
    import coderadar

    graph = coderadar.CodeGraph()
    mq = MacrameQuery(graph)
    edge_types = [e.strip() for e in edges.split(",")] if edges else None
    results = mq.traverse(start_id, depth, edge_types, direction)

    if not results:
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
    import coderadar

    graph = coderadar.CodeGraph()
    mq = MacrameQuery(graph)
    results = mq.callers_of(entity_id)

    if not results:
        console.print(f"[yellow]No callers found for {entity_id}[/yellow]")
        return

    console.print(f"[bold]Callers of {entity_id}:[/bold]")
    for r in results:
        console.print(f"  {r.get('id', r.get('entity_id', '?'))} "
                      f"({r.get('file', '?')}:{r.get('line', '?')})")


@main.command()
@click.argument("entity_id")
def callees(entity_id: str):
    """List callees called by an entity."""
    from .query import MacrameQuery
    import coderadar

    graph = coderadar.CodeGraph()
    mq = MacrameQuery(graph)
    results = mq.callees_of(entity_id)

    if not results:
        console.print(f"[yellow]No callees from {entity_id}[/yellow]")
        return

    console.print(f"[bold]Callees from {entity_id}:[/bold]")
    for r in results:
        console.print(f"  {r.get('id', r.get('entity_id', '?'))} "
                      f"({r.get('file', '?')}:{r.get('line', '?')})")


@main.command()
def shell():
    """REPL with persistent graph in memory."""
    console.print("[bold]CodeRadar Shell[/bold]")
    console.print("Type 'help' for commands, 'exit' to quit.")
    import coderadar
    graph = coderadar.CodeGraph()

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
            for row in graph.query(query_str):
                console.print(row)
        elif cmd.startswith("traverse "):
            start_id = cmd[9:].strip()
            from .query import MacrameQuery
            for row in MacrameQuery(graph).traverse(start_id):
                console.print(row)
        elif cmd.startswith("callers "):
            entity_id = cmd[8:].strip()
            from .query import MacrameQuery
            for row in MacrameQuery(graph).callers_of(entity_id):
                console.print(row)
        elif cmd.strip() == "stats":
            console.print(graph.stats())


@main.command()
@click.argument("path", type=click.Path())
@click.option("--format", "fmt", default="bin", help="bin, json, yaml")
def export(path: str, fmt: str):
    """Export snapshot."""
    import coderadar
    graph = coderadar.CodeGraph()
    try:
        graph.export_snapshot(path)
        console.print(f"[green]Snapshot exported to {path}[/green]")
    except NotImplementedError as e:
        console.print(f"[yellow]export_snapshot is not implemented:[/yellow] {e}")


@main.command()
@click.argument("snapshot", type=click.Path(exists=True))
def load_snapshot(snapshot: str):
    """Load and verify snapshot integrity."""
    import coderadar
    try:
        graph = coderadar.load(snapshot)
        stats = graph.stats()
        console.print(f"[green]Snapshot loaded: {stats}[/green]")
    except NotImplementedError as e:
        console.print(f"[yellow]load_snapshot is not implemented:[/yellow] {e}")


@main.command()
@click.option("--full", is_flag=True, help="Full re-index of all files")
def rebuild(full: bool):
    """Full re-index of all files (or incremental)."""
    console.print(f"[bold]Rebuilding[/bold] {'(full)' if full else '(incremental)'}...")


@main.command()
def stats():
    """Counts, parse-error summary, memory usage."""
    import coderadar
    graph = coderadar.CodeGraph()
    s = graph.stats()
    table = Table(title="Graph Statistics")
    table.add_column("Metric", style="cyan")
    table.add_column("Value", style="green")
    for k, v in s.items():
        table.add_row(k, str(v))
    console.print(table)


@main.command()
@click.argument("viz_type")
@click.argument("args", nargs=-1)
@click.option("--output", "-o", type=click.Path())
@click.option("--format", "fmt", default="mermaid")
def visualize(viz_type: str, args: tuple, output: Optional[str], fmt: str):
    """Run a visualizer: hierarchy, dependencies, call-graph."""
    from .visualizers.mermaid import generate_mermaid
    from .visualizers.graphviz_viz import generate_dot
    from .visualizers.call_graph import generate_call_graph
    import coderadar

    graph = coderadar.CodeGraph()
    arg_list = list(args)

    if viz_type == "hierarchy":
        if fmt == "graphviz" or fmt == "dot":
            text = generate_dot("hierarchy", arg_list, graph)
        else:
            text = generate_mermaid("hierarchy", arg_list, graph)
    elif viz_type == "dependencies":
        if fmt == "graphviz" or fmt == "dot":
            text = generate_dot("dependencies", arg_list, graph)
        else:
            text = generate_mermaid("dependencies", arg_list, graph)
    elif viz_type == "call-graph":
        text = generate_call_graph(arg_list, graph)
    else:
        console.print(f"[red]Unknown visualization type: {viz_type}[/red]")
        return

    if output:
        Path(output).write_text(text)
        console.print(f"[green]Written to {output}[/green]")
    else:
        console.print(text)


@main.command()
@click.option("--last", type=int, default=20, help="Number of recent mutations")
def mutations(last: int):
    """Audit trail from MutationLog."""
    console.print(f"[bold]Last {last} mutations:[/bold]")


@main.command()
@click.option("--unresolved", is_flag=True, help="Show all unresolved references")
@click.option("--low-confidence", is_flag=True, help="List edges below min_confidence")
def diagnose(unresolved: bool, low_confidence: bool):
    """Show unresolved references or low-confidence edges."""
    if unresolved:
        console.print("[bold]Unresolved references:[/bold]")
    if low_confidence:
        console.print("[bold]Low-confidence edges:[/bold]")


@main.command()
def status():
    """Daemon health check."""
    console.print("[green]CodeRadar is running[/green]")


@main.group()
def mcp():
    """Model Context Protocol server commands."""


@mcp.command()
@click.option("--path", "project_path", type=click.Path(exists=True), default=".",
              help="Project root to serve.")
def serve(project_path: str):
    """Start the CodeRadar MCP server over stdio.

    Connect an MCP client (Claude Code, Cursor, etc.) to this server to get
    code intelligence over the indexed project. The server exposes four tools:
    codegraph_explore, codegraph_node, codegraph_search, codegraph_affected.

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

    _activate(project_path)

    # Load the code graph by running analyze() on the project root.
    # This re-reads all source files, detects changes since last index,
    # and ensures the in-memory GLOBAL_GRAPH is always fresh.
    # One-time startup cost (seconds) — the server is long-lived.
    print(f"Indexing {project_path}...", file=sys.stderr)
    graph = coderadar.analyze(project_path)
    try:
        from coderadar._core import graph_stats
        stats = graph_stats()
        print(f"Loaded: {stats.get('modules', 0)} modules, "
              f"{stats.get('functions', 0)} functions, "
              f"{stats.get('call_edges', 0)} call edges", file=sys.stderr)
    except ImportError:
        print("MCP server ready", file=sys.stderr)
    mcp_serve(graph)


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
    except ImportError:
        clean = True

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
    from coderadar import CodeGraph

    watch_paths = list(paths) if paths else ["src/", "tests/"]
    graph = CodeGraph()
    watcher = graph.watch(watch_paths, debounce_ms=debounce)
    console.print(f"[bold green]Watching:[/bold green] {', '.join(watch_paths)}")
    console.print("[dim]Press Ctrl+C to stop[/dim]")
    watcher.run_forever()


if __name__ == "__main__":
    main()
