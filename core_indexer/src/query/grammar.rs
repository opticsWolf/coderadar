// CodeRadar v3.6 — Query Engine: Pest Grammar (§7.1)
// See grammar.pest for the full pest grammar definition.

use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "query/grammar.pest"]
pub struct QueryParser;

/// AST node for a parsed query.
#[derive(Clone, Debug)]
pub struct ParsedQuery {
    pub entity: EntityType,
    pub select: Vec<SelectItem>,
    pub where_clause: Option<Predicate>,
    pub group_by: Vec<String>,
    pub order_by: Option<OrderBy>,
    pub limit: Option<u64>,
}

#[derive(Clone, Debug)]
pub enum EntityType {
    Modules,
    Classes,
    Functions,
    Imports,
    Calls,
    Fields,
}

#[derive(Clone, Debug)]
pub enum SelectItem {
    Path(String),
    Aggregate {
        func: AggFunc,
        path: String,
        alias: String,
    },
}

#[derive(Clone, Debug)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Clone, Debug)]
pub enum Predicate {
    Comparison {
        left: Operand,
        op: CompOp,
        right: Operand,
    },
    Not(Box<Predicate>),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
}

#[derive(Clone, Debug)]
pub enum Operand {
    Path(Vec<String>),
    StringValue(String),
    NumberValue(f64),
    BoolValue(bool),
    ListValue(Vec<Operand>),
    DerivedCall {
        name: String,
        args: Vec<Operand>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum CompOp {
    Eq,
    NotEq,
    LessEq,
    GreaterEq,
    Less,
    Greater,
    Contains,
    Matches,
    In,
}

#[derive(Clone, Debug)]
pub struct OrderBy {
    pub path: String,
    pub direction: OrderDir,
}

#[derive(Clone, Debug)]
pub enum OrderDir {
    Asc,
    Desc,
}

/// Parse a query string using the Pest grammar.
pub fn parse_query(query_str: &str) -> Result<ParsedQuery, String> {
    let mut pairs = QueryParser::parse(Rule::query, query_str)
        .map_err(|e| format!("Parse error: {:?}", e))?;

    // The first (and only) top-level pair is the full query
    let query_pair = pairs.next().ok_or("Empty parse result")?;

    let mut entity = EntityType::Functions;
    let mut select = Vec::new();
    let mut where_clause = None;
    let mut group_by = Vec::new();
    let mut order_by = None;
    let mut limit = None;

    for pair in query_pair.into_inner() {
        let rule = pair.as_rule();
        match rule {
            Rule::WHITESPACE | Rule::COMMENT => {}
            Rule::entity => {
                entity = parse_entity_type(pair);
            }
            Rule::select_clause => {
                select = parse_select_clause(pair);
            }
            Rule::where_clause => {
                where_clause = Some(parse_where_clause(pair));
            }
            Rule::group_by_clause => {
                group_by = parse_group_by(pair);
            }
            Rule::order_by_clause => {
                let ob = parse_order_by(pair);
                // If parse_order_by returns None, the order_by stays None
                if ob.is_some() {
                    order_by = ob;
                }
            }
            Rule::limit_clause => {
                limit = parse_limit(pair);
            }
            _ => {
                // Debug: what rule wasn't matched?
                let _ = pair.as_rule();
            }
        }
    }

    Ok(ParsedQuery {
        entity,
        select,
        where_clause,
        group_by,
        order_by,
        limit,
    })
}

fn parse_entity_type(pair: Pair<Rule>) -> EntityType {
    match pair.as_str() {
        "modules" => EntityType::Modules,
        "classes" => EntityType::Classes,
        "functions" => EntityType::Functions,
        "imports" => EntityType::Imports,
        "calls" => EntityType::Calls,
        "fields" => EntityType::Fields,
        _ => EntityType::Functions,
    }
}

fn parse_select_clause(pair: Pair<Rule>) -> Vec<SelectItem> {
    pair.into_inner()
        .filter(|p| p.as_rule() == Rule::select_item)
        .map(|p| {
            let inner = p.into_inner().next().unwrap();
            match inner.as_rule() {
                Rule::agg_expr => {
                    // agg_expr = agg_func "(" (path | "*") ")" "as" identifier
                    // Pest only produces pairs for rule refs, not literals.
                    // Inner pairs: [agg_func, path_or_star, identifier]
                    // But "*" literal may or may not produce a pair.
                    // Conservative: collect all, find path/star and alias.
                    let parts: Vec<_> = inner.into_inner().collect();
                    let func_part = &parts[0];
                    let agg_func = match func_part.as_str() {
                        "count" => AggFunc::Count,
                        "sum" => AggFunc::Sum,
                        "avg" => AggFunc::Avg,
                        "min" => AggFunc::Min,
                        "max" => AggFunc::Max,
                        _ => AggFunc::Count,
                    };
                    // Second-to-last is the path/star, last is the alias identifier
                    let n = parts.len();
                    if n >= 2 {
                        let arg = parts[n - 2].as_str().to_string();
                        let alias = parts[n - 1].as_str().to_string();
                        SelectItem::Aggregate { func: agg_func, path: arg, alias }
                    } else {
                        SelectItem::Path(String::new())
                    }
                }
                Rule::path => SelectItem::Path(inner.as_str().to_string()),
                _ => SelectItem::Path(inner.as_str().to_string()),
            }
        })
        .collect()
}

fn parse_where_clause(pair: Pair<Rule>) -> Predicate {
    let inner = pair.into_inner().next().unwrap();
    parse_or_expr(inner)
}

fn parse_or_expr(pair: Pair<Rule>) -> Predicate {
    let mut parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return parse_and_expr(parts.remove(0));
    }
    let left = parse_and_expr(parts.remove(0));
    let right = parse_or_expr(parts.remove(1)); // skip "or" token
    Predicate::Or(Box::new(left), Box::new(right))
}

fn parse_and_expr(pair: Pair<Rule>) -> Predicate {
    let mut parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return parse_atom(parts.remove(0));
    }
    let left = parse_atom(parts.remove(0));
    let right = parse_and_expr(parts.remove(1)); // skip "and" token
    Predicate::And(Box::new(left), Box::new(right))
}

fn parse_atom(pair: Pair<Rule>) -> Predicate {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::predicate => parse_predicate(inner),
        Rule::not_atom => {
            let sub = inner.into_inner().next().unwrap();
            Predicate::Not(Box::new(parse_atom(sub)))
        }
        _ => parse_predicate(inner),
    }
}

fn parse_predicate(pair: Pair<Rule>) -> Predicate {
    let mut parts: Vec<_> = pair.into_inner().collect();
    let left = parse_operand(parts.remove(0));
    let op = parse_comp_op(parts.remove(0));
    let right = parse_operand(parts.remove(0));
    Predicate::Comparison { left, op, right }
}

fn parse_operand(pair: Pair<Rule>) -> Operand {
    match pair.as_rule() {
        Rule::path => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| p.as_str().to_string())
                .collect();
            if parts.len() == 1 {
                Operand::Path(parts)
            } else {
                Operand::Path(parts)
            }
        }
        Rule::string => {
            let s = pair.as_str();
            Operand::StringValue(s[1..s.len() - 1].to_string())
        }
        Rule::number => {
            Operand::NumberValue(pair.as_str().parse().unwrap_or(0.0))
        }
        Rule::bool => {
            Operand::BoolValue(pair.as_str() == "true")
        }
        Rule::list => Operand::ListValue(
            pair.into_inner()
                .filter(|p| p.as_rule() == Rule::value)
                .map(parse_operand)
                .collect(),
        ),
        Rule::derived_call => {
            let mut parts = pair.into_inner();
            let name = parts.next().unwrap().as_str().to_string();
            let args: Vec<Operand> = parts.map(parse_operand).collect();
            Operand::DerivedCall { name, args }
        }
        _ => Operand::StringValue(pair.as_str().to_string()),
    }
}

fn parse_comp_op(pair: Pair<Rule>) -> CompOp {
    match pair.as_str() {
        "==" => CompOp::Eq,
        "!=" => CompOp::NotEq,
        "<=" => CompOp::LessEq,
        ">=" => CompOp::GreaterEq,
        "<" => CompOp::Less,
        ">" => CompOp::Greater,
        "contains" => CompOp::Contains,
        "matches" => CompOp::Matches,
        "in" => CompOp::In,
        _ => CompOp::Eq,
    }
}

fn parse_group_by(pair: Pair<Rule>) -> Vec<String> {
    pair.into_inner()
        .filter(|p| p.as_rule() == Rule::path)
        .map(|p| p.as_str().to_string())
        .collect()
}

fn parse_order_by(pair: Pair<Rule>) -> Option<OrderBy> {
    // Pest doesn't produce pairs for literal strings like "order", "by".
    // Inner pairs: [path, direction?]
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.is_empty() {
        return None;
    }
    let path = parts[0].as_str().to_string();
    let direction = if parts.len() > 1 {
        match parts[1].as_str() {
            "desc" => OrderDir::Desc,
            _ => OrderDir::Asc,
        }
    } else {
        OrderDir::Asc
    };
    Some(OrderBy { path, direction })
}

fn parse_limit(pair: Pair<Rule>) -> Option<u64> {
    pair.into_inner()
        .next()
        .and_then(|p| p.as_str().parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Query Parsing ─────────────────────────────────────────────────

    #[test]
    fn test_parse_simple_query() {
        let q = parse_query("functions").expect("should parse");
        assert!(matches!(q.entity, EntityType::Functions));
        assert!(q.select.is_empty());
        assert!(q.where_clause.is_none());
    }

    #[test]
    fn test_parse_class_query() {
        let q = parse_query("classes").unwrap();
        assert!(matches!(q.entity, EntityType::Classes));
    }

    #[test]
    fn test_parse_with_where() {
        let q = parse_query("functions where is_async == true").unwrap();
        assert!(matches!(q.entity, EntityType::Functions));
        assert!(q.where_clause.is_some());
    }

    #[test]
    fn test_parse_with_limit() {
        let q = parse_query("functions limit 10").unwrap();
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn test_parse_with_order_by() {
        let q = parse_query("classes order by method_count desc").unwrap();
        // For debugging: print all inner rules seen by parse_query
        let pairs = QueryParser::parse(Rule::query, "classes order by method_count desc").unwrap();
        let qp = pairs.into_iter().next().unwrap();
        let inner: Vec<_> = qp.into_inner().map(|p| format!("{:?}", p.as_rule())).collect();
        eprintln!("Inner rules: {:?}", inner);

        assert!(q.order_by.is_some(), "order_by is None. Inner rules: {:?}", inner);
        if let Some(order) = q.order_by {
            assert_eq!(order.path, "method_count");
        }
    }

    #[test]
    fn test_parse_with_select() {
        let q = parse_query(
            "functions select name, count(*) as cnt group by module.name"
        ).unwrap();
        assert!(!q.select.is_empty());
        assert!(!q.group_by.is_empty());
        assert_eq!(q.group_by[0], "module.name");
    }

    #[test]
    fn test_parse_combined_clauses() {
        let q = parse_query(
            "classes where method_count > 5 order by method_count desc limit 25"
        ).unwrap();
        assert!(q.where_clause.is_some());
        assert!(q.order_by.is_some());
        assert_eq!(q.limit, Some(25));
    }

    #[test]
    fn test_parse_with_contains() {
        let q = parse_query(
            "functions where decorators contains \"deprecated\""
        ).unwrap();
        assert!(q.where_clause.is_some());
    }

    #[test]
    fn test_parse_with_not() {
        let q = parse_query(
            "functions where not is_async == true"
        ).unwrap();
        assert!(q.where_clause.is_some());
    }

    #[test]
    fn test_parse_all_entities() {
        for entity in &["modules", "classes", "functions", "imports", "calls", "fields"] {
            let q = parse_query(entity).unwrap_or_else(|_| panic!("Failed: {}", entity));
            assert!(matches!(
                q.entity,
                EntityType::Modules
                    | EntityType::Classes
                    | EntityType::Functions
                    | EntityType::Imports
                    | EntityType::Calls
                    | EntityType::Fields
            ));
        }
    }

    #[test]
    fn test_parse_derived_call() {
        let q = parse_query(
            "functions where has_method(\"__init__\") == true"
        ).unwrap();
        assert!(q.where_clause.is_some());
    }

    // ── CompOp Parsing ───────────────────────────────────────────────

    #[test]
    fn test_comp_op_all_variants() {
        use pest::Parser;
        for (input, expected) in &[
            ("==", CompOp::Eq),
            ("!=", CompOp::NotEq),
            ("<=", CompOp::LessEq),
            (">=", CompOp::GreaterEq),
            ("<", CompOp::Less),
            (">", CompOp::Greater),
            ("contains", CompOp::Contains),
            ("matches", CompOp::Matches),
            ("in", CompOp::In),
        ] {
            let pairs = QueryParser::parse(Rule::comp_op, input).unwrap();
            let op = parse_comp_op(pairs.into_iter().next().unwrap());
            assert_eq!(op, *expected, "Failed for input: {}", input);
        }
    }

    #[test]
    fn test_parse_invalid_query_returns_err() {
        let result = parse_query("garbage ###");
        assert!(result.is_err());
    }
}
