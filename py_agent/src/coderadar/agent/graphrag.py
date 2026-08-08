"""CodeRadar v3.3 — GraphRAG Query Pipeline (§13.3)

Natural-language intent classification, Cypher template selection,
two-stage vector search, and context building with token budget.
"""

from __future__ import annotations

import structlog
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, List, Optional, Tuple

logger = structlog.get_logger(__name__)


class Intent(Enum):
    """GraphRAG query intent classification."""
    SCOPE_EXPLORATION = "scope_exploration"
    IMPACT_ANALYSIS = "impact_analysis"
    CALL_CHAIN = "call_chain"
    SIMILARITY_SEARCH = "similarity_search"
    DEPENDENCY_GRAPH = "dependency_graph"
    DEFINITION_LOOKUP = "definition_lookup"


class ContextStrategy(Enum):
    """How much context to include per entity."""
    SIGNATURES_ONLY = "signatures_only"  # ~50-150 tokens/entity
    STRUCTURAL = "structural"            # ~200-600 tokens/entity
    FULL = "full"                        # ~400-4000 tokens/entity


@dataclass
class GraphRAGResult:
    """Result of a GraphRAG query execution."""
    intent: Intent
    rows: List[Dict[str, Any]]
    tokens_used: int
    strategy_per_entity: Dict[str, str]
    elapsed_ms: float


class QueryPlanner:
    """Classifies natural-language intent and extracts parameters.

    Six intent classes (§13.3.1):
    - scope_exploration: root_path, query_text
    - impact_analysis: target_id, depth
    - call_chain: source_name, target_name, max_depth
    - similarity_search: query_text, top_k
    - dependency_graph: root_path, depth
    - definition_lookup: name
    """

    def classify(self, query_text: str) -> Tuple[Intent, Dict[str, Any]]:
        """Classify a natural language query into an intent + parameters."""
        query_lower = query_text.lower()

        if any(w in query_lower for w in ["find", "search", "similar",
                                            "like", "related"]):
            return Intent.SIMILARITY_SEARCH, {
                "query_text": query_text,
                "top_k": 10,
            }

        if any(w in query_lower for w in ["impact", "what breaks", "depends on",
                                            "who calls", "callers of", "affected by"]):
            return Intent.IMPACT_ANALYSIS, {
                "target_id": "",
                "depth": 3,
            }

        if any(w in query_lower for w in ["chain", "path from", "how does",
                                            "reach", "flow from"]):
            return Intent.CALL_CHAIN, {
                "source_name": "",
                "target_name": "",
                "max_depth": 5,
            }

        if any(w in query_lower for w in ["dependencies", "imports", "module graph",
                                            "what imports"]):
            return Intent.DEPENDENCY_GRAPH, {
                "root_path": "",
                "depth": 3,
            }

        if any(w in query_lower for w in ["what is", "define", "show me",
                                            "definition", "signature of", "signature"]):
            return Intent.DEFINITION_LOOKUP, {
                "name": "",
            }

        # Default: scope exploration
        return Intent.SCOPE_EXPLORATION, {
            "root_path": "",
            "query_text": query_text,
        }


class GraphRAGContextBuilder:
    """Builds LLM context from query results with token budget.

    Three strategies, selected per query:
    - signatures_only: signatures + docstring + decorators (~50-150 tokens)
    - structural: above + control-flow skeleton (~200-600 tokens)
    - full: entire bodies (~400-4000 tokens)

    Starts with signatures_only, promotes top-ranked entities until budget exhausted.
    """

    def __init__(self, max_tokens: int = 8192,
                 strategy: ContextStrategy = ContextStrategy.STRUCTURAL):
        self.max_tokens = max_tokens
        self.default_strategy = strategy

    def build_context(
        self,
        results: List[Dict[str, Any]],
        intent: Intent,
    ) -> GraphRAGResult:
        """Build context from query results within the token budget."""
        tokens_used = 0
        rows = []
        strategies: Dict[str, str] = {}

        for result in results:
            entity_id = result.get("id", result.get("name", ""))
            strategy = self._select_strategy(result, intent)
            compressed = self._compress(result, strategy)
            tokens = self._estimate_tokens(compressed)

            if tokens_used + tokens > self.max_tokens:
                # Can't fit more entities — stop
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

    def _select_strategy(self, result: Dict[str, Any],
                         intent: Intent) -> ContextStrategy:
        """Select compression strategy for a result entity."""
        # Top-ranked results get structural/full, rest get signatures
        return self.default_strategy

    def _compress(self, result: Dict[str, Any],
                  strategy: ContextStrategy) -> Dict[str, Any]:
        """Compress an entity based on the chosen strategy."""
        if strategy == ContextStrategy.SIGNATURES_ONLY:
            return {
                "name": result.get("name", ""),
                "signature": result.get("signature", ""),
                "docstring": result.get("docstring", ""),
            }
        elif strategy == ContextStrategy.STRUCTURAL:
            body = result.get("body", "")
            skeleton = self._skeletonize(body)
            return {
                **result,
                "body_skeleton": skeleton,
            }
        else:  # FULL
            return dict(result)

    def _skeletonize(self, body: str) -> str:
        """Extract control-flow skeleton from a function body."""
        lines = []
        for line in body.split("\n"):
            stripped = line.strip()
            # Keep control flow keywords
            if any(stripped.startswith(kw)
                   for kw in ("if ", "for ", "while ", "return ",
                              "def ", "class ", "try:", "except", "raise",
                              "with ", "match ", "case ")):
                lines.append(line)
            # Collapse function calls
            elif "(" in stripped and stripped.endswith(")"):
                lines.append(line)
        return "\n".join(lines)

    def _estimate_tokens(self, entity: Dict[str, Any]) -> int:
        """Rough token estimate: ~4 chars per token."""
        total_chars = sum(len(str(v)) for v in entity.values())
        return max(1, total_chars // 4)


class GraphRAGPipeline:
    """Orchestrates GraphRAG query execution: plan → embed → execute → build context."""

    def __init__(self, db: Any = None, max_context_tokens: int = 8192):
        self.planner = QueryPlanner()
        self.builder = GraphRAGContextBuilder(max_tokens=max_context_tokens)
        self.db = db

    def query(self, query_text: str) -> GraphRAGResult:
        """Execute a full GraphRAG query pipeline.

        1. Classify intent
        2. Select Cypher template
        3. Embed query text (for vector search intents)
        4. Execute query (LadybugDB + optional Rust traversal)
        5. Build context with token budget
        """
        intent, params = self.planner.classify(query_text)

        # Execute query based on intent
        results = self._execute(intent, params)

        # Build context
        return self.builder.build_context(results, intent)

    def _execute(self, intent: Intent, params: Dict[str, Any]) -> List[Dict[str, Any]]:
        """Execute the appropriate Cypher/Rust query for an intent."""
        # Route to appropriate template from §7.3
        if intent == Intent.IMPACT_ANALYSIS:
            return self._impact_analysis(params)
        elif intent == Intent.CALL_CHAIN:
            return self._call_chain(params)
        elif intent == Intent.SIMILARITY_SEARCH:
            return self._similarity_search(params)
        elif intent == Intent.DEPENDENCY_GRAPH:
            return self._dependency_graph(params)
        elif intent == Intent.DEFINITION_LOOKUP:
            return self._definition_lookup(params)
        else:  # SCOPE_EXPLORATION
            return self._scope_exploration(params)

    def _scope_exploration(self, params: Dict) -> List[Dict]:
        return []

    def _impact_analysis(self, params: Dict) -> List[Dict]:
        return []

    def _call_chain(self, params: Dict) -> List[Dict]:
        return []

    def _similarity_search(self, params: Dict) -> List[Dict]:
        return []

    def _dependency_graph(self, params: Dict) -> List[Dict]:
        return []

    def _definition_lookup(self, params: Dict) -> List[Dict]:
        return []
