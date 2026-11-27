//! Pure-Rust port of `agent/graphrag.py` and `query/planner.py`.
//!
//! The Python version wraps `MacrameQuery`; here the same primitives are
//! already `core_indexer::api` functions, so the pipeline stays in the engine
//! and the Rust app only needs a thin CLI/MCP front door.

use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::types::ProjectedGraph;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryIntent {
    ScopeExploration,
    ImpactAnalysis,
    CallChain,
    SimilaritySearch,
    DependencyGraph,
    DefinitionLookup,
}
impl QueryIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScopeExploration => "scope_exploration",
            Self::ImpactAnalysis => "impact_analysis",
            Self::CallChain => "call_chain",
            Self::SimilaritySearch => "similarity_search",
            Self::DependencyGraph => "dependency_graph",
            Self::DefinitionLookup => "definition_lookup",
        }
    }
}

#[derive(Clone, Debug)]
pub struct QueryPlan {
    pub intent: QueryIntent,
    pub method: String,
    pub params: Map<String, Value>,
}
impl QueryPlan {
    pub fn json(&self) -> Value {
        json!({
            "intent": self.intent.as_str(),
            "method": self.method,
            "params": Value::Object(self.params.clone()),
        })
    }
}

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// Classify a natural-language query with the Python planner's precedence.
pub fn plan_query(text: &str) -> QueryPlan {
    let lower = text.to_lowercase();
    let mut params = Map::new();
    params.insert("query_text".into(), Value::String(text.to_string()));
    let (intent, method, extra) = if has_any(
        &lower,
        &[
            "impact",
            "what breaks",
            "depends on",
            "who calls",
            "callers of",
            "affected by",
        ],
    ) {
        (
            QueryIntent::ImpactAnalysis,
            "callers_of",
            vec![("depth", json!(3))],
        )
    } else if has_any(
        &lower,
        &["chain", "path from", "how does", "reach", "flow from"],
    ) {
        (
            QueryIntent::CallChain,
            "traverse",
            vec![("max_depth", json!(5)), ("edge_types", json!(["CALLS"]))],
        )
    } else if has_any(&lower, &["find", "search", "similar", "like", "related"]) {
        (
            QueryIntent::SimilaritySearch,
            "search_similar",
            vec![("top_k", json!(10))],
        )
    } else if has_any(
        &lower,
        &["dependencies", "imports", "module graph", "what imports"],
    ) {
        (
            QueryIntent::DependencyGraph,
            "traverse",
            vec![("depth", json!(3)), ("edge_types", json!(["IMPORTS"]))],
        )
    } else if has_any(
        &lower,
        &["what is", "define", "show me", "definition", "signature of"],
    ) {
        (QueryIntent::DefinitionLookup, "find", vec![])
    } else {
        (
            QueryIntent::ScopeExploration,
            "list_by_kind",
            vec![("kind", json!("function"))],
        )
    };
    for (key, value) in extra {
        params.insert(key.to_string(), value);
    }
    QueryPlan {
        intent,
        method: method.to_string(),
        params,
    }
}

fn snapshot() -> Result<std::sync::Arc<ProjectedGraph>, String> {
    let guard = crate::GLOBAL_GRAPH.read();
    let graph = guard
        .as_ref()
        .ok_or_else(|| "No graph loaded — run coderadar init first".to_string())?;
    Ok(graph.snapshot())
}

fn find_one(snap: &ProjectedGraph, query: &str) -> Option<Value> {
    crate::api::search(snap, query, 1, None)
        .ok()?
        .into_iter()
        .next()
}

fn edge_kinds(params: &Map<String, Value>) -> Vec<String> {
    params
        .get("edge_types")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn execute_plan(plan: &QueryPlan) -> Result<Vec<Value>, String> {
    let snap = snapshot()?;
    let query = plan
        .params
        .get("query_text")
        .and_then(Value::as_str)
        .unwrap_or("");
    let limit = plan
        .params
        .get("top_k")
        .and_then(Value::as_u64)
        .unwrap_or(10) as usize;
    match plan.method.as_str() {
        "callers_of" => {
            let Some(entity) = find_one(&snap, query) else {
                return Ok(Vec::new());
            };
            let id = entity.get("id").and_then(Value::as_str).unwrap_or("");
            Ok(crate::api::callers(&snap, id))
        }
        "traverse" => {
            let Some(entity) = find_one(&snap, query) else {
                return Ok(Vec::new());
            };
            let id = entity.get("id").and_then(Value::as_str).unwrap_or("");
            let depth = plan
                .params
                .get("max_depth")
                .or_else(|| plan.params.get("depth"))
                .and_then(Value::as_u64)
                .unwrap_or(3) as usize;
            Ok(crate::api::traverse(
                &snap,
                id,
                "both",
                &edge_kinds(&plan.params),
                depth,
            ))
        }
        "search_similar" => {
            let config = crate::active_config();
            let vectors = crate::api::embed_texts(
                &config.embedding.model,
                &[query.to_string()],
                config.embedding.batch_size,
            )?;
            let Some(vector) = vectors.first() else {
                return Ok(Vec::new());
            };
            let value = crate::api::search_similar_vec(vector, limit)?;
            value
                .as_array()
                .cloned()
                .ok_or_else(|| "EngineError: similarity search returned no array".to_string())
        }
        "find" => Ok(find_one(&snap, query).into_iter().collect()),
        "list_by_kind" => {
            let kind = plan.params.get("kind").and_then(Value::as_str);
            crate::api::search(&snap, "", limit, kind)
        }
        other => Err(format!("EngineError: unknown GraphRAG method {other:?}")),
    }
}

fn skeletonize(body: &str) -> String {
    const PREFIXES: &[&str] = &[
        "if ", "for ", "while ", "return ", "def ", "class ", "try:", "except", "raise", "with ",
        "match ", "case ",
    ];
    body.lines()
        .filter(|line| {
            let trimmed = line.trim();
            PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix))
                || (trimmed.contains('(') && trimmed.ends_with(')'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compress(row: &Value, structural: bool) -> Value {
    if !structural {
        let mut out = Map::new();
        for key in ["name", "signature", "docstring"] {
            if let Some(value) = row.get(key) {
                out.insert(key.to_string(), value.clone());
            }
        }
        return Value::Object(out);
    }
    let mut out = row.as_object().cloned().unwrap_or_default();
    if let Some(body) = row.get("body").and_then(Value::as_str) {
        out.insert("body_skeleton".into(), Value::String(skeletonize(body)));
    }
    Value::Object(out)
}

fn estimate_tokens(row: &Value) -> usize {
    let chars: usize = match row {
        Value::Object(map) => map
            .values()
            .map(|value| match value {
                Value::String(text) => text.len(),
                other => other.to_string().len(),
            })
            .sum(),
        other => other.to_string().len(),
    };
    chars.div_ceil(4).max(1)
}

/// Run the full classify → execute → budgeted-context pipeline.
///
/// # Errors
/// Returns the engine message when no graph is loaded or a search fails.
pub fn graphrag_context(query: &str, max_tokens: usize) -> Result<Value, String> {
    let started = Instant::now();
    let plan = plan_query(query);
    let results = execute_plan(&plan)?;
    let mut rows = Vec::new();
    let mut strategies = Map::new();
    let mut used = 0usize;
    for row in results {
        let compressed = compress(&row, true);
        let tokens = estimate_tokens(&compressed);
        if used + tokens > max_tokens {
            break;
        }
        used += tokens;
        let id = row
            .get("id")
            .or_else(|| row.get("entity_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        strategies.insert(id, Value::String("structural".into()));
        rows.push(compressed);
    }
    Ok(json!({
        "intent": plan.intent.as_str(),
        "method": plan.method,
        "query": query,
        "rows": rows,
        "tokens_used": used,
        "strategy_per_entity": Value::Object(strategies),
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
    }))
}

/// Parse `coderadar-rs context QUERY [--budget N]` in the engine.
pub fn cli(args: &[String]) -> Result<Value, String> {
    let mut budget = 8192usize;
    let mut terms = Vec::new();
    let mut iter = args.iter();
    while let Some(value) = iter.next() {
        if value == "--budget" {
            budget = iter
                .next()
                .and_then(|next| next.parse::<usize>().ok())
                .unwrap_or(budget);
        } else if !value.starts_with('-') {
            terms.push(value.clone());
        }
    }
    graphrag_context(&terms.join(" "), budget)
}

/// Parse MCP `coderadar_context` arguments in the engine.
pub fn mcp(args: &Value) -> Result<Value, String> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let budget = args
        .get("max_tokens")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(8192);
    graphrag_context(query, budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_precedence_matches_python() {
        assert_eq!(
            plan_query("who calls save").intent,
            QueryIntent::ImpactAnalysis
        );
        assert_eq!(
            plan_query("path from a to b").intent,
            QueryIntent::CallChain
        );
        assert_eq!(
            plan_query("similar to foo").intent,
            QueryIntent::SimilaritySearch
        );
        assert_eq!(
            plan_query("module dependencies").intent,
            QueryIntent::DependencyGraph
        );
        assert_eq!(
            plan_query("what is foo").intent,
            QueryIntent::DefinitionLookup
        );
        assert_eq!(
            plan_query("functions").intent,
            QueryIntent::ScopeExploration
        );
    }

    #[test]
    fn skeleton_keeps_signature_and_control_lines() {
        let body = "def f(x):\n    y = x + 1\n    if y:\n        return y\n";
        assert_eq!(skeletonize(body), "def f(x):\n    if y:\n        return y");
    }
}
