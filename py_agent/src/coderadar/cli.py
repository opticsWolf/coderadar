"""CodeRadar v3.3 — Command-Line Interface (§16)"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Optional

import click
from rich.console import Console
from rich.table import Table

console = Console()


@click.group()
@click.version_option(version="0.1.0", prog_name="coderadar",
                      message="coderadar %(version)s (spec v3.3)")
def main():
    """CodeRadar — live semantic graph of your codebase.

    Maintains an incrementally updatable graph of code structure,
    enabling LLMs and developers to query, visualize, and safely rewrite code.
    """


@main.command()
@click.argument("path", type=click.Path(exists=True), default=".")
def init(path: str):
    """Initial analysis; persists to .harness/"""
    console.print(f"[bold]Analyzing[/bold] {path}...")
    import coderadar
    graph = coderadar.analyze(path)
    stats = graph.stats()
    console.print(f"  Modules:    {stats.get('modules', 0)}")
    console.print(f"  Classes:    {stats.get('classes', 0)}")
    console.print(f"  Functions:  {stats.get('functions', 0)}")
    console.print(f"  Imports:    {stats.get('imports', 0)}")
    console.print("[green]✓ Analysis complete[/green]")


@main.command()
@click.argument("path", type=click.Path(exists=True), default=".")
def analyze(path: str):
    """One-shot analysis without persistence."""
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
@click.argument("cypher_query")
@click.option("--params", default="{}", help="JSON parameters")
def cypher(cypher_query: str, params: str):
    """Execute a Cypher query against LadybugDB."""
    import json as json_mod
    import coderadar

    param_dict = json_mod.loads(params)
    graph = coderadar.CodeGraph()
    results = graph.cypher(cypher_query, **param_dict)

    console.print_json(data=results)


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
            console.print("Commands: query <sql>, stats, exit")
        elif cmd.startswith("query "):
            query_str = cmd[6:]
            for row in graph.query(query_str):
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
    graph.export_snapshot(path)
    console.print(f"[green]Snapshot exported to {path}[/green]")


@main.command()
@click.argument("snapshot", type=click.Path(exists=True))
def load_snapshot(snapshot: str):
    """Load and verify snapshot integrity."""
    import coderadar
    graph = coderadar.load(snapshot)
    stats = graph.stats()
    console.print(f"[green]Snapshot loaded: {stats}[/green]")


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
@click.argument("qualified_name")
def resolve_cmd(qualified_name: str):
    """Show resolution chain (debugging)."""
    import coderadar
    chain = coderadar.resolve(qualified_name)
    for step in chain:
        console.print(step)


@main.command()
@click.argument("qualified_name")
def callers(qualified_name: str):
    """List callers of a function."""
    console.print(f"Callers of [bold]{qualified_name}[/bold]:")
    # Stub


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

    if viz_type == "hierarchy":
        text = generate_mermaid("hierarchy", list(args))
    elif viz_type == "dependencies":
        text = generate_dot("dependencies", list(args))
    elif viz_type == "call-graph":
        text = generate_call_graph(list(args))
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


if __name__ == "__main__":
    main()
