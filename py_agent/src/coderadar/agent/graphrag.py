"""CodeRadar v3.6 — GraphRAG Query Pipeline (§13.3)

Natural-language intent classification → direct Macrame operations →
context building with token budget.

No Cypher; intents route directly to MacrameQuery primitives:
  - traverse() for call chains and dependency graphs
  - search_similar() for vector similarity
  - find() for definition lookup
  - callers_of() for impact analysis
"""

from __future__ import annotations

import structlog
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, List, Tuple

from ..query import MacrameQuery
from ..query.planner import QueryIntent, QueryPlan, plan_query

logger = structlog.get_logger(__name__)


class ContextStrategy(Enum):
    SIGNATURES_ONLY = "signatures_only"
    STRUCTURAL = "structural"
    FULL = "full"


@dataclass
class GraphRAGResult:
    """Result of a GraphRAG query execution."""
    intent: QueryIntent
    rows: List[Dict[str, Any]]
    tokens_used: int
    strategy_per_entity: Dict[str, str]
    elapsed_ms: float


class GraphRAGContextBuilder:
    """Builds LLM context from Macrame query results with token budget."""

    def __init__(self, max_tokens: int = 8192,
                 strategy: ContextStrategy = ContextStrategy.STRUCTURAL):
        self.max_tokens = max_tokens
        self.default_strategy = strategy

    def build_context(
        self, results: List[Dict[str, Any]], intent: QueryIntent,
    ) -> GraphRAGResult:
        tokens_used = 0
        rows: List[Dict[str, Any]] = []
        strategies: Dict[str, str] = {}

        for result in results:
            entity_id = result.get("id", result.get("entity_id", ""))
            strategy = self.default_strategy
            compressed = self._compress(result, strategy)
            tokens = self._estimate_tokens(compressed)

            if tokens_used + tokens > self.max_tokens:
                break

            tokens_used += tokens
            rows.append(compressed)
            strategies[entity_id] = strategy.value

        return GraphRAGResult(
            intent=intent,
            rows=rows,
            tokens_used=tokens_used,
            strategy_per_entity=strategies,
            elapsed_ms=0.0,
        )

    def _compress(self, result: Dict[str, Any],
                  strategy: ContextStrategy) -> Dict[str, Any]:
        if strategy == ContextStrategy.SIGNATURES_ONLY:
            return {
                "name": result.get("name", ""),
                "signature": result.get("signature", ""),
                "docstring": result.get("docstring", ""),
            }
        elif strategy == ContextStrategy.STRUCTURAL:
            body = result.get("body", "")
            return {**result, "body_skeleton": self._skeletonize(body)}
        else:
            return dict(result)

    def _skeletonize(self, body: str) -> str:
        lines = []
        for line in body.split("\n"):
            stripped = line.strip()
            if any(stripped.startswith(kw)
                   for kw in ("if ", "for ", "while ", "return ",
                              "def ", "class ", "try:", "except", "raise",
                              "with ", "match ", "case ")):
                lines.append(line)
            elif "(" in stripped and stripped.endswith(")"):
                lines.append(line)
        return "\n".join(lines)

    def _estimate_tokens(self, entity: Dict[str, Any]) -> int:
        total_chars = sum(len(str(v)) for v in entity.values())
        return max(1, total_chars // 4)


class GraphRAGPipeline:
    """Orchestrates GraphRAG: classify → MacrameQuery → build context.

    Usage:
        pipeline = GraphRAGPipeline(graph)
        result = pipeline.query("who calls User.save?")
        for row in result.rows:
            print(row["name"])
    """

    def __init__(self, graph: Any = None, max_context_tokens: int = 8192):
        self.query = MacrameQuery(graph)
        self.builder = GraphRAGContextBuilder(max_tokens=max_context_tokens)
        self.graph = graph

    def execute(self, query_text: str) -> GraphRAGResult:
        """Execute a full GraphRAG query pipeline.

        1. Classify intent via QueryPlanner
        2. Execute via MacrameQuery primitive
        3. Build context with token budget
        """
        import time
        start = time.monotonic()

        plan = plan_query(query_text)
        results = self._execute_plan(plan)
        result = self.builder.build_context(results, plan.intent)

        elapsed = (time.monotonic() - start) * 1000
        logger.debug("graphrag.done", intent=plan.intent.value,
                      rows=len(result.rows), elapsed_ms=elapsed)
        return result

    def _execute_plan(self, plan: QueryPlan) -> List[Dict[str, Any]]:
        """Execute a QueryPlan via the appropriate MacrameQuery method."""
        method = plan.method
        params = plan.params

        if method == "callers_of":
            return self.query.callers_of(
                params.get("entity_id", params.get("target_id", "")))
        elif method == "traverse":
            return self.query.traverse(
                start_id=params.get("start_id", ""),
                max_depth=params.get("max_depth", params.get("depth", 3)),
                edge_types=params.get("edge_types"),
            )
        elif method == "search_similar":
            return self.query.search_similar(
                query_embedding=params.get("embedding", []),
                top_k=params.get("top_k", 10),
            )
        elif method == "find":
            entity = self.query.find(params.get("entity_id", ""))
            return [entity] if entity else []
        elif method == "list_by_kind":
            return self.query.list_by_kind(
                kind=params.get("kind", "function"),
                limit=params.get("limit", 100),
            )
        else:
            return []


class QueryPlanner:
    """Natural-language query intent classifier.

    Wraps plan_query with a class-based API that returns
    (QueryIntent, params) for the GraphRAG pipeline entry point.

    Usage:
        planner = QueryPlanner()
        intent, params = planner.classify("who calls create_user")
        # → (QueryIntent.IMPACT_ANALYSIS, {"query_text": "...", "depth": 3})
    """

    def classify(self, query_text: str) -> tuple[QueryIntent, dict[str, Any]]:
        """Classify a natural-language query into intent + parameters.

        Args:
            query_text: The natural language query to classify.

        Returns:
            Tuple of (QueryIntent, params dict) for routing to MacrameQuery.
        """
        plan = plan_query(query_text)
        return plan.intent, plan.params
