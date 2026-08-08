"""CodeRadar v3.5 — Query Planner (§13.3)

Classifies natural-language queries into intents, routing each to the
appropriate Macrame primitive: traversal, similarity search, concept
lookup, impact analysis, or dependency exploration.

No Cypher — intents map directly to MacrameQuery operations.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, Optional


class QueryIntent(str, Enum):
    """Six query intents, each mapping to a MacrameQuery method."""
    SCOPE_EXPLORATION = "scope_exploration"    # traverse or list_by_kind
    IMPACT_ANALYSIS = "impact_analysis"         # traverse(reverse, CALLS)
    CALL_CHAIN = "call_chain"                   # traverse(CALLS)
    SIMILARITY_SEARCH = "similarity_search"     # search_similar
    DEPENDENCY_GRAPH = "dependency_graph"       # traverse(IMPORTS)
    DEFINITION_LOOKUP = "definition_lookup"     # find


@dataclass
class QueryPlan:
    """A planned Macrame query with bound parameters.

    Routes to the appropriate MacrameQuery method:
    - SCOPE_EXPLORATION → query.list_by_kind() + query.traverse()
    - IMPACT_ANALYSIS   → query.callers_of() + recursive traverse
    - CALL_CHAIN         → query.traverse(edge_types=["CALLS"])
    - SIMILARITY_SEARCH  → query.search_similar()
    - DEPENDENCY_GRAPH   → query.traverse(edge_types=["IMPORTS"])
    - DEFINITION_LOOKUP  → query.find()
    """
    intent: QueryIntent
    method: str                    # MacrameQuery method name
    params: Dict[str, Any] = field(default_factory=dict)
    top_k: int = 10


def plan_query(query_text: str) -> QueryPlan:
    """Classify a natural-language query into an intent + Macrame method.

    Args:
        query_text: The natural language query to classify.

    Returns:
        A QueryPlan routing to the appropriate MacrameQuery method.
    """
    query_lower = query_text.lower()

    # Impact analysis: "what breaks if I change X", "who calls Y"
    if any(w in query_lower for w in ["impact", "what breaks", "depends on",
                                        "who calls", "callers of", "affected by"]):
        return QueryPlan(
            intent=QueryIntent.IMPACT_ANALYSIS,
            method="callers_of",
            params={"query_text": query_text, "depth": 3},
        )

    # Call chain: "path from A to B", "how does A reach B"
    if any(w in query_lower for w in ["chain", "path from", "how does",
                                        "reach", "flow from"]):
        return QueryPlan(
            intent=QueryIntent.CALL_CHAIN,
            method="traverse",
            params={"query_text": query_text, "max_depth": 5,
                    "edge_types": ["CALLS"]},
        )

    # Similarity search: "find functions like", "similar to"
    if any(w in query_lower for w in ["find", "search", "similar",
                                        "like", "related"]):
        return QueryPlan(
            intent=QueryIntent.SIMILARITY_SEARCH,
            method="search_similar",
            params={"query_text": query_text, "top_k": 10},
        )

    # Dependency graph: "module dependencies", "imports of"
    if any(w in query_lower for w in ["dependencies", "imports", "module graph",
                                        "what imports"]):
        return QueryPlan(
            intent=QueryIntent.DEPENDENCY_GRAPH,
            method="traverse",
            params={"query_text": query_text, "depth": 3,
                    "edge_types": ["IMPORTS"]},
        )

    # Definition lookup: "what is X", "show me Y", "define Z"
    if any(w in query_lower for w in ["what is", "define", "show me",
                                        "definition", "signature of"]):
        return QueryPlan(
            intent=QueryIntent.DEFINITION_LOOKUP,
            method="find",
            params={"query_text": query_text},
        )

    # Default: scope exploration
    return QueryPlan(
        intent=QueryIntent.SCOPE_EXPLORATION,
        method="list_by_kind",
        params={"query_text": query_text},
    )
