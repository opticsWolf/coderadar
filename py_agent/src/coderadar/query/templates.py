"""CodeRadar v3.3 — Cypher Query Templates (§7.3)

Parameterized Cypher templates for LadybugDB.
Each template maps to a GraphRAG intent class.
"""

from __future__ import annotations

from typing import Any, Dict

# ── Scope Exploration with pre-filtered vector search ───────────────────────

SCOPE_EXPLORATION = """
MATCH (root:File {path: $root_path})
OPTIONAL MATCH (root)-[:IMPORTS*1..2]->(dep:File)
WITH collect(DISTINCT dep) + [root] AS scope_files
UNWIND scope_files AS sf
MATCH (sf)-[:DECLARES_FUNC]->(target:Function)
WITH collect(DISTINCT target.id) AS candidate_ids
CALL db_similarity_search('func_embedding_idx', $query_embedding, $top_k,
                          {filter: {id: candidate_ids}})
YIELD node AS matched, score
OPTIONAL MATCH (matched)<-[r:CALLS]-(caller:Function)
OPTIONAL MATCH (matched)-[r2:CALLS]->(callee:Function)
WHERE r.confidence > 0.7
RETURN matched.name, matched.signature, matched.body, matched.docstring,
       parent.path AS file_path, score, collect(DISTINCT callee.name) AS calls
ORDER BY score DESC
LIMIT $top_k
"""

# ── Impact Analysis (reverse dependency) ────────────────────────────────────

IMPACT_ANALYSIS = """
MATCH (target:Function {id: $target_id})
MATCH (caller:Function)-[:CALLS*1..$depth]->(target)
WITH DISTINCT caller
MATCH (caller)-[r:CALLS]->(other:Function)
RETURN caller.name, caller.signature, caller.body, parent_file.path,
       collect(DISTINCT other.name) AS also_calls
ORDER BY parent_file.path
"""

# ── Call Chain ──────────────────────────────────────────────────────────────

CALL_CHAIN = """
MATCH path = (src:Function {name: $source_name})-[:CALLS*1..$max_depth]->(tgt:Function {name: $target_name})
WITH path, length(path) AS depth
ORDER BY depth
LIMIT 5
UNWIND nodes(path) AS node
WITH collect(node.name) AS chain, depth
RETURN chain, depth
"""

# ── Global Similarity Search ────────────────────────────────────────────────

SIMILARITY_SEARCH = """
CALL db_similarity_search('func_embedding_idx', $query_embedding, $top_k)
YIELD node AS matched, score
OPTIONAL MATCH (matched)-[r:CALLS]->(callees:Function)
RETURN matched.name, matched.signature, matched.body, matched.docstring,
       parent.path, score, collect(DISTINCT callees.name) AS calls
ORDER BY score DESC
"""

# ── Dependency Graph ────────────────────────────────────────────────────────

DEPENDENCY_GRAPH = """
MATCH (root:File {path: $root_path})-[imp:IMPORTS*1..$depth]->(dep:File)
OPTIONAL MATCH (dep)-[:DECLARES_CLASS]->(cls:Class)
OPTIONAL MATCH (dep)-[:DECLARES_FUNC]->(func:Function)
RETURN dep.path, dep.language,
       collect(DISTINCT cls.name) AS classes,
       collect(DISTINCT func.name) AS functions,
       length(imp) AS depth
ORDER BY depth, dep.path
"""

# ── Definition Lookup ───────────────────────────────────────────────────────

DEFINITION_LOOKUP = """
MATCH (func:Function)
WHERE func.name = $name OR func.qualified_name = $name
OPTIONAL MATCH (func)-[:HAS_PARAM]->(param:Parameter)
OPTIONAL MATCH (func)-[r:CALLS]->(callees:Function)
OPTIONAL MATCH (callers:Function)-[r2:CALLS]->(func)
RETURN func.name, func.signature, func.body, func.docstring, parent.path,
       collect(DISTINCT param.name) AS parameters,
       collect(DISTINCT callees.name) AS calls,
       collect(DISTINCT callers.name) AS called_by
"""

# ── Template Registry ──────────────────────────────────────────────────────

CYPHER_TEMPLATES: Dict[str, str] = {
    "scope_exploration": SCOPE_EXPLORATION.strip(),
    "impact_analysis": IMPACT_ANALYSIS.strip(),
    "call_chain": CALL_CHAIN.strip(),
    "similarity_search": SIMILARITY_SEARCH.strip(),
    "dependency_graph": DEPENDENCY_GRAPH.strip(),
    "definition_lookup": DEFINITION_LOOKUP.strip(),
}


def get_template(template_id: str) -> str:
    """Get a Cypher template by ID.

    Args:
        template_id: One of the template keys.

    Returns:
        The parameterized Cypher query string.

    Raises:
        ValueError: If the template_id is unknown.
    """
    template = CYPHER_TEMPLATES.get(template_id)
    if template is None:
        raise ValueError(f"Unknown template: {template_id}. "
                         f"Available: {list(CYPHER_TEMPLATES.keys())}")
    return template
