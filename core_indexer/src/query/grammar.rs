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
    /// `functions where kind == 'method'` — the plan §3.4 alias.
    Methods,
    Constants,
    /// Every name-bearing kind: modules, classes, functions, constants.
    Entities,
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
    DerivedCall { name: String, args: Vec<Operand> },
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
    StartsWith,
    EndsWith,
    In,
}

#[derive(Clone, Debug)]
pub struct OrderBy {
    pub path: String,
    pub direction: OrderDir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrderDir {
    Asc,
    Desc,
}

/// Parse a query string using the Pest grammar.
pub fn parse_query(query_str: &str) -> Result<ParsedQuery, String> {
    let mut pairs =
        QueryParser::parse(Rule::query, query_str).map_err(|e| format_parse_error(&e))?;

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

    let parsed = ParsedQuery {
        entity,
        select,
        where_clause,
        group_by,
        order_by,
        limit,
    };
    // §3.2: unknown fields are a compile-time error, not an empty result.
    crate::query::schema::validate_query(&parsed)?;
    Ok(parsed)
}

/// Turn a Pest parse error into something a user can act on (plan §3.5).
///
/// Pest's default `Debug` output is a struct dump —
/// `ParsingError { positives: [atom], negatives: [] }` — which tells a user
/// nothing. Rule names are mapped to what they mean in the language, with the
/// offending line and a caret under the failure column.
fn format_parse_error(err: &pest::error::Error<Rule>) -> String {
    use pest::error::{ErrorVariant, LineColLocation};
    let (line_no, col) = match err.line_col {
        LineColLocation::Pos(pos) => pos,
        LineColLocation::Span(start, _) => start,
    };
    let expected = match &err.variant {
        ErrorVariant::ParsingError {
            positives,
            negatives,
        } => {
            let mut hints: Vec<&str> = positives.iter().map(|r| rule_hint(*r)).collect();
            hints.sort_unstable();
            hints.dedup();
            let mut msg = if hints.is_empty() {
                "unexpected input".to_string()
            } else {
                format!("expected {}", join_hints(&hints))
            };
            let neg: Vec<&str> = negatives
                .iter()
                .map(|r| rule_hint(*r))
                .filter(|h| !h.is_empty())
                .collect();
            if !neg.is_empty() {
                msg.push_str(&format!(" (not {})", join_hints(&neg)));
            }
            msg
        }
        ErrorVariant::CustomError { message } => message.clone(),
    };
    let snippet = err.line().trim_end();
    let caret = format!("{}^", " ".repeat(col.saturating_sub(1)));
    format!("Parse error at line {line_no}, column {col}: {expected}\n  {snippet}\n  {caret}")
}

/// What a grammar rule means to someone writing a query.
fn rule_hint(rule: Rule) -> &'static str {
    match rule {
        Rule::comp_op => {
            "a comparison operator (==, !=, <, <=, >, >=, contains, matches, starts_with, ends_with, in)"
        }
        Rule::atom | Rule::predicate => "a field comparison, `not`, or a parenthesized condition",
        Rule::operand | Rule::value => {
            "a field name or a value (string, number, true/false, null, list)"
        }
        Rule::identifier | Rule::path => "a field name",
        Rule::entity => {
            "an entity kind (modules, classes, functions, methods, constants, entities, imports, calls, fields)"
        }
        Rule::select_item | Rule::select_clause => "a field name or an aggregate",
        Rule::agg_expr => "an aggregate such as count(*) as n",
        Rule::order_by_clause => "`order by <field> [asc|desc]`",
        Rule::group_by_clause => "`group by <field>`",
        Rule::limit_clause => "`limit <number>`",
        Rule::EOI => "the end of the query",
        _ => "a valid query clause",
    }
}

fn join_hints(hints: &[&str]) -> String {
    match hints {
        [] => String::new(),
        [one] => one.to_string(),
        [a, b] => format!("{a} or {b}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

fn parse_entity_type(pair: Pair<Rule>) -> EntityType {
    match pair.as_str() {
        "modules" => EntityType::Modules,
        "classes" => EntityType::Classes,
        "functions" => EntityType::Functions,
        "methods" => EntityType::Methods,
        "constants" => EntityType::Constants,
        "entities" => EntityType::Entities,
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
                    // Pest only produces pairs for rule refs, not literals, so
                    // `count(*)` yields just [agg_func, identifier] — reading
                    // the argument off the pair list made it the string
                    // "count". The source text between the parens is the
                    // source of truth.
                    let text = inner.as_str();
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
                    let alias = parts
                        .last()
                        .map(|p| p.as_str().to_string())
                        .unwrap_or_default();
                    let open = text.find('(').map(|i| i + 1).unwrap_or(0);
                    let close = text.rfind(')').unwrap_or(text.len());
                    let arg = text[open.min(close)..close].trim().to_string();
                    SelectItem::Aggregate {
                        func: agg_func,
                        path: arg,
                        alias,
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
    let parts: Vec<_> = pair.into_inner().collect();
    let mut iter = parts.into_iter();
    let first = iter.next().expect("or_expr has no children");
    let mut acc = parse_and_expr(first);
    for next in iter {
        // "or" string literal is silent in pest, so each remaining pair is
        // a full and_expr — left-associate them into a chain of Or.
        acc = Predicate::Or(Box::new(acc), Box::new(parse_and_expr(next)));
    }
    acc
}

fn parse_and_expr(pair: Pair<Rule>) -> Predicate {
    let parts: Vec<_> = pair.into_inner().collect();
    let mut iter = parts.into_iter();
    let first = iter.next().expect("and_expr has no children");
    let mut acc = parse_atom(first);
    for next in iter {
        // "and" string literal is silent in pest, so each remaining pair is
        // a full atom — left-associate them into a chain of And.
        acc = Predicate::And(Box::new(acc), Box::new(parse_atom(next)));
    }
    acc
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
            // `path` is an atomic pest rule (@{ ... }), so into_inner() is
            // empty; read the raw text and split on '.' to recover the parts.
            let s = pair.as_str();
            let parts: Vec<String> = s.split('.').map(|p| p.to_string()).collect();
            Operand::Path(parts)
        }
        Rule::string => {
            let s = pair.as_str();
            Operand::StringValue(s[1..s.len() - 1].to_string())
        }
        Rule::number => Operand::NumberValue(pair.as_str().parse().unwrap_or(0.0)),
        Rule::bool => Operand::BoolValue(pair.as_str() == "true"),
        Rule::null => Operand::StringValue("null".to_string()),
        Rule::list => Operand::ListValue(
            pair.into_inner()
                .filter(|p| {
                    p.as_rule() == Rule::value
                        || p.as_rule() == Rule::string
                        || p.as_rule() == Rule::number
                        || p.as_rule() == Rule::bool
                })
                .map(parse_operand)
                .collect(),
        ),
        Rule::derived_call => {
            let mut parts = pair.into_inner();
            let name = parts.next().unwrap().as_str().to_string();
            let args: Vec<Operand> = parts.map(parse_operand).collect();
            Operand::DerivedCall { name, args }
        }
        // operand / value are non-silent wrappers in the grammar — recurse
        // into their single inner pair so `name == "x"` resolves the field
        // path instead of being treated as a string literal.
        Rule::operand | Rule::value => {
            let raw = pair.as_str().to_string();
            match pair.into_inner().next() {
                Some(inner) => parse_operand(inner),
                None => Operand::StringValue(raw),
            }
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
        "starts_with" => CompOp::StartsWith,
        "ends_with" => CompOp::EndsWith,
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
    // Inner pairs: [path, order_dir?]
    let parts: Vec<_> = pair.into_inner().collect();
    let path = parts
        .iter()
        .find(|p| p.as_rule() == Rule::path)?
        .as_str()
        .to_string();
    let direction = match parts
        .iter()
        .find(|p| p.as_rule() == Rule::order_dir)
        .map(|p| p.as_str())
    {
        Some("desc") => OrderDir::Desc,
        _ => OrderDir::Asc,
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
        let order = q.order_by.expect("order_by is None");
        assert_eq!(order.path, "method_count");
        assert_eq!(order.direction, OrderDir::Desc);
    }

    #[test]
    fn test_order_by_direction_is_not_swallowed() {
        // `desc` used to be an inline literal in the grammar, so Pest matched
        // it and emitted no pair for it — every ORDER BY came back ascending.
        let asc = parse_query("classes order by name asc")
            .unwrap()
            .order_by
            .unwrap();
        assert_eq!(asc.direction, OrderDir::Asc);

        let bare = parse_query("classes order by name")
            .unwrap()
            .order_by
            .unwrap();
        assert_eq!(
            bare.direction,
            OrderDir::Asc,
            "no direction means ascending"
        );

        let desc = parse_query("functions order by line desc")
            .unwrap()
            .order_by
            .unwrap();
        assert_eq!(desc.path, "line");
        assert_eq!(desc.direction, OrderDir::Desc);
    }

    #[test]
    fn test_dotted_unknown_paths_are_rejected() {
        // `module.name` used to parse and silently evaluate to Null on
        // `functions` (no nested field resolution exists) — §3.2 makes it an
        // error instead of an empty result.
        let err = parse_query("functions order by module.name desc").unwrap_err();
        assert!(err.contains("unknown field 'module.name'"), "{err}");
    }

    #[test]
    fn test_parse_with_select() {
        let q = parse_query("functions select name, count(*) as cnt group by kind").unwrap();
        assert!(!q.select.is_empty());
        assert!(!q.group_by.is_empty());
        assert_eq!(q.group_by[0], "kind");
        // `count(*)` must aggregate everything — not the field named "count".
        match &q.select[1] {
            SelectItem::Aggregate { func, path, alias } => {
                assert!(matches!(func, AggFunc::Count));
                assert_eq!(path, "*");
                assert_eq!(alias, "cnt");
            }
            other => panic!("expected an aggregate, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_combined_clauses() {
        let q = parse_query("classes where method_count > 5 order by method_count desc limit 25")
            .unwrap();
        assert!(q.where_clause.is_some());
        assert!(q.order_by.is_some());
        assert_eq!(q.limit, Some(25));
    }

    #[test]
    fn test_parse_with_contains() {
        let q = parse_query("functions where decorators contains \"deprecated\"").unwrap();
        assert!(q.where_clause.is_some());
    }

    #[test]
    fn test_parse_path_field_has_parts() {
        // Regression: atomic `path` must yield Path(["name"]), not Path([]).
        let q = parse_query("functions where name == \"parse\"").unwrap();
        match q.where_clause.expect("where clause") {
            Predicate::Comparison {
                left,
                op: CompOp::Eq,
                right,
            } => {
                match left {
                    Operand::Path(parts) => assert_eq!(parts, vec!["name".to_string()]),
                    other => panic!("expected Path, got {:?}", other),
                }
                match right {
                    Operand::StringValue(s) => assert_eq!(s, "parse"),
                    other => panic!("expected StringValue, got {:?}", other),
                }
            }
            other => panic!("expected Comparison, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_with_and_chain() {
        // Regression: `a and b` must fold into And(a, b), not panic.
        let q = parse_query("functions where name == \"parse\" and is_async == false").unwrap();
        assert!(matches!(q.where_clause, Some(Predicate::And(_, _))));
    }

    #[test]
    fn test_parse_single_quoted_string() {
        // Regression: single-quoted strings must parse like double-quoted.
        let q = parse_query("functions where name contains 'parse'").unwrap();
        match q.where_clause.expect("where clause") {
            Predicate::Comparison {
                left,
                op: CompOp::Contains,
                right,
            } => {
                assert!(matches!(left, Operand::Path(_)));
                match right {
                    Operand::StringValue(s) => assert_eq!(s, "parse"),
                    other => panic!("expected StringValue, got {:?}", other),
                }
            }
            other => panic!("expected Comparison, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_with_or_chain() {
        let q =
            parse_query("functions where name == \"a\" or name == \"b\" or name == \"c\"").unwrap();
        // left-assoc: Or(Or(a, b), c)
        match q.where_clause.expect("where clause") {
            Predicate::Or(outer, inner_c) => {
                assert!(matches!(*outer, Predicate::Or(_, _)));
                assert!(matches!(*inner_c, Predicate::Comparison { .. }));
            }
            other => panic!("expected Or chain, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_with_not() {
        let q = parse_query("functions where not is_async == true").unwrap();
        assert!(q.where_clause.is_some());
    }

    #[test]
    fn test_parse_all_entities() {
        for entity in &[
            "modules",
            "classes",
            "functions",
            "imports",
            "calls",
            "fields",
        ] {
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
        let q = parse_query("functions where has_method(\"__init__\") == true").unwrap();
        assert!(q.where_clause.is_some());
    }

    #[test]
    fn parse_errors_are_human_readable() {
        // §3.5: no more `ParsingError { positives: [atom], negatives: [] }`.
        let err = parse_query("functions where name").unwrap_err();
        assert!(err.starts_with("Parse error at line 1, column "), "{err}");
        assert!(
            err.contains("expected a comparison operator"),
            "the failure is a missing operator: {err}"
        );
        assert!(err.contains("functions where name"), "snippet shown: {err}");
        assert!(err.contains('^'), "caret marks the column: {err}");
        assert!(!err.contains("ParsingError"), "no raw Pest structs: {err}");

        // A bad entity kind names the kinds that would work.
        let err = parse_query("functionz where name == 1").unwrap_err();
        assert!(err.contains("expected an entity kind"), "{err}");
        assert!(err.contains("classes"), "{err}");
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
