"""Query planner: intent classification for natural-language queries.

The keyword ladder lives in Rust (`core_indexer::api::plan_query`); this
module is the data-shaped view Python callers expect.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from .. import plan_query as _plan_query


class QueryIntent(str, Enum):
    """Six query intents, each mapping to an engine method."""

    SCOPE_EXPLORATION = "scope_exploration"
    IMPACT_ANALYSIS = "impact_analysis"
    CALL_CHAIN = "call_chain"
    SIMILARITY_SEARCH = "similarity_search"
    DEPENDENCY_GRAPH = "dependency_graph"
    DEFINITION_LOOKUP = "definition_lookup"


@dataclass
class QueryPlan:
    """A planned query with bound parameters."""

    intent: QueryIntent
    method: str
    params: dict[str, Any] = field(default_factory=dict)
    top_k: int = 10


def plan_query(query_text: str) -> QueryPlan:
    """Classify a natural-language query into an intent + engine method."""
    report = _plan_query(query_text)
    return QueryPlan(
        intent=QueryIntent(report["intent"]),
        method=report["method"],
        params=dict(report["params"]),
        top_k=int(report["top_k"]),
    )


__all__ = ["QueryIntent", "QueryPlan", "plan_query"]
