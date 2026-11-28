"""Renderers that the Rust engine already produces.

Only the helpers with a Rust equivalent live here; the CLI text renderers
stay in the binary (`coderadar <command>`).
"""

from . import (
    apply_output_budget,
    extract_path_from_header,
    mutation_applied,
    mutation_error,
    mutation_plan,
    query_usage,
    render_entity_code,
    stale_banner,
    trim_to_char_budget,
)

__all__ = [
    "apply_output_budget", "extract_path_from_header", "mutation_applied",
    "mutation_error", "mutation_plan", "query_usage", "render_entity_code",
    "stale_banner", "trim_to_char_budget",
]
