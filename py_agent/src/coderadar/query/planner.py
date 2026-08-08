"""CodeRadar v3.3 — Query Planner (§13.3)
Classifies natural-language queries into one of six intents.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, Optional


class QueryIntent(str, Enum):
    SCOPE_EXPLORATION = "scope_exploration"
    IMPACT_ANALYSIS = "impact_analysis"
    CALL_CHAIN = "call_chain"
    SIMILARITY_SEARCH = "similarity_search"
    DEPENDENCY_GRAPH = "dependency_graph"
    DEFINITION_LOOKUP = "definition_lookup"


@dataclass
class QueryPlan:
    """A planned Cypher query with bound parameters."""
    intent: QueryIntent
    template_id: str
    params: Dict[str, Any] = field(default_factory=dict)
    top_k: int = 10


def plan_query(query_text: str, graph_epoch: int = 0) -> QueryPlan:
    """Classify a natural-language query into an intent and extract parameters.

    Args:
        query_text: The natural language query to classify.
        graph_epoch: Current graph epoch for cache keying.

    Returns:
        A QueryPlan with the selected intent, template, and parameters.
    """
    query_lower = query_text.lower()

    # Impact analysis: "what breaks if I change X", "who calls Y"
    if any(w in query_lower for w in ["impact", "what breaks", "depends on",
                                        "who calls", "callers of", "affected by"]):
        return QueryPlan(
            intent=QueryIntent.IMPACT_ANALYSIS,
            template_id="impact_analysis",
            params={"query_text": query_text, "depth": 3},
        )

    # Call chain: "path from A to B", "how does A reach B"
    if any(w in query_lower for w in ["chain", "path from", "how does",
                                        "reach", "flow from"]):
        return QueryPlan(
            intent=QueryIntent.CALL_CHAIN,
            template_id="call_chain",
            params={"query_text": query_text, "max_depth": 5},
        )

    # Similarity search: "find functions like", "similar to"
    if any(w in query_lower for w in ["find", "search", "similar",
                                        "like", "related"]):
        return QueryPlan(
            intent=QueryIntent.SIMILARITY_SEARCH,
            template_id="similarity_search",
            params={"query_text": query_text, "top_k": 10},
        )

    # Dependency graph: "module dependencies", "imports of"
    if any(w in query_lower for w in ["dependencies", "imports", "module graph",
                                        "what imports"]):
        return QueryPlan(
            intent=QueryIntent.DEPENDENCY_GRAPH,
            template_id="dependency_graph",
            params={"query_text": query_text, "depth": 3},
        )

    # Definition lookup: "what is X", "show me Y", "define Z"
    if any(w in query_lower for w in ["what is", "define", "show me",
                                        "definition", "signature of"]):
        return QueryPlan(
            intent=QueryIntent.DEFINITION_LOOKUP,
            template_id="definition_lookup",
            params={"query_text": query_text},
        )

    # Default: scope exploration
    return QueryPlan(
        intent=QueryIntent.SCOPE_EXPLORATION,
        template_id="scope_exploration",
        params={"query_text": query_text},
    )
