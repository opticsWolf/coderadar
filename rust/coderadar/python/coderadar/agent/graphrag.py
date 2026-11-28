"""GraphRAG context pipeline (P2 agent-context seam).

Planning, execution and the token-budgeted compression all run in Rust
(`api::plan_query`, `api::graphrag_execute`, `api::graphrag_build_context`).
This module supplies the data shapes Python callers expect.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from .. import graphrag_build_context as _build_context, graphrag_execute as _execute
from ..query.planner import QueryIntent, QueryPlan, plan_query

__all__ = [
    "ContextStrategy",
    "GraphRAGContextBuilder",
    "GraphRAGPipeline",
    "GraphRAGResult",
    "QueryPlanner",
]


class ContextStrategy(Enum):
    SIGNATURES_ONLY = "signatures_only"
    STRUCTURAL = "structural"
    FULL = "full"


@dataclass
class GraphRAGResult:
    """Result of a GraphRAG query execution."""

    intent: QueryIntent
    rows: list[dict[str, Any]]
    tokens_used: int
    strategy_per_entity: dict[str, str] = field(default_factory=dict)
    elapsed_ms: float = 0.0


def _result(report: dict[str, Any]) -> GraphRAGResult:
    return GraphRAGResult(
        intent=QueryIntent(report["intent"]),
        rows=list(report["rows"]),
        tokens_used=int(report["tokens_used"]),
        strategy_per_entity=dict(report["strategy_per_entity"]),
        elapsed_ms=float(report["elapsed_ms"]),
    )


class GraphRAGContextBuilder:
    """Builds LLM context from engine rows with a token budget."""

    def __init__(
        self,
        max_tokens: int = 8192,
        strategy: ContextStrategy = ContextStrategy.STRUCTURAL,
    ):
        self.max_tokens = max_tokens
        self.default_strategy = strategy

    def build_context(
        self, results: list[dict[str, Any]], intent: QueryIntent
    ) -> GraphRAGResult:
        """Compress rows to the budget (Rust owns the strategy ladder)."""
        report = _build_context(
            json.dumps(list(results)),
            intent.value if isinstance(intent, QueryIntent) else str(intent),
            self.max_tokens,
            self.default_strategy.value,
        )
        return _result(report)


class GraphRAGPipeline:
    """Classify -> execute -> budget the context, all in Rust."""

    def __init__(self, graph: Any = None, max_context_tokens: int = 8192):
        self.graph = graph
        self.max_context_tokens = max_context_tokens
        self.builder = GraphRAGContextBuilder(max_tokens=max_context_tokens)

    def execute(self, query_text: str) -> GraphRAGResult:
        """Execute a full GraphRAG query pipeline.

        # Errors
        Raises `RuntimeError` when no graph is loaded.
        """
        return _result(_execute(query_text, self.max_context_tokens, "structural"))


class QueryPlanner:
    """Natural-language query intent classifier (engine-backed)."""

    def classify(self, query_text: str) -> tuple[QueryIntent, dict[str, Any]]:
        """Return the intent and its parameters for the engine route."""
        plan: QueryPlan = plan_query(query_text)
        return plan.intent, plan.params
