"""CodeRadar MCP — Explore Budget Scaling (§26.3)

Adaptive output budgets scaled to project size. Smaller projects get tighter
caps; larger projects get generous caps because the agent's native discovery
cost dwarfs a fat explore call.

Invariant: a larger tier must never get a smaller per-file budget than a
smaller tier.
"""

from dataclasses import dataclass


@dataclass
class ExploreBudget:
    """Adaptive budget for codegraph_explore responses."""
    max_output_chars: int       # Hard cap on total output characters
    max_calls: int              # Max explore calls per session
    default_max_files: int      # Default maxFiles when caller didn't specify
    max_chars_per_file: int     # Cap per file (monotonic across tiers)
    max_symbols_in_header: int  # Max symbols listed per file header
    include_relationships: bool
    include_additional_files: bool
    include_completeness_signal: bool
    include_budget_note: bool


def get_explore_budget(file_count: int) -> ExploreBudget:
    """Compute the adaptive explore budget for a given project size.

    Breakpoints from CodeGraph's production tiers (§26.3 / tools.ts).
    """
    if file_count < 150:
        return ExploreBudget(
            max_output_chars=13000,
            max_calls=1,
            default_max_files=4,
            max_chars_per_file=3800,
            max_symbols_in_header=5,
            include_relationships=False,
            include_additional_files=False,
            include_completeness_signal=False,
            include_budget_note=False,
        )
    if file_count < 500:
        return ExploreBudget(
            max_output_chars=18000,
            max_calls=1,
            default_max_files=5,
            max_chars_per_file=3800,
            max_symbols_in_header=6,
            include_relationships=False,
            include_additional_files=False,
            include_completeness_signal=False,
            include_budget_note=False,
        )
    if file_count < 5000:
        return ExploreBudget(
            max_output_chars=24000,
            max_calls=2,
            default_max_files=8,
            max_chars_per_file=6500,
            max_symbols_in_header=10,
            include_relationships=True,
            include_additional_files=True,
            include_completeness_signal=True,
            include_budget_note=False,
        )
    if file_count < 15000:
        return ExploreBudget(
            max_output_chars=28000,
            max_calls=3,
            default_max_files=10,
            max_chars_per_file=8000,
            max_symbols_in_header=12,
            include_relationships=True,
            include_additional_files=True,
            include_completeness_signal=True,
            include_budget_note=True,
        )
    if file_count < 25000:
        return ExploreBudget(
            max_output_chars=35000,
            max_calls=4,
            default_max_files=12,
            max_chars_per_file=10000,
            max_symbols_in_header=15,
            include_relationships=True,
            include_additional_files=True,
            include_completeness_signal=True,
            include_budget_note=True,
        )
    # ≥ 25,000
    return ExploreBudget(
        max_output_chars=38000,
        max_calls=5,
        default_max_files=15,
        max_chars_per_file=12000,
        max_symbols_in_header=20,
        include_relationships=True,
        include_additional_files=True,
        include_completeness_signal=True,
        include_budget_note=True,
    )
