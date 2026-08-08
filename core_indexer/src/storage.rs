// CodeRadar v3.5 — Macrame Storage Interface (§10)
// Maps CodeRadar entities and edges to Macrame concepts and assertions.
// Entity lifecycle: upsert → retire → supersede (never delete).

use crate::types::*;

/// Annotation keys for CodeRadar metadata on Macrame concepts.
pub mod annotation {
    pub const KIND: &str = "kind";
    pub const FILE_PATH: &str = "file_path";
    pub const LANGUAGE: &str = "language";
    pub const LINE: &str = "line";
    pub const END_LINE: &str = "end_line";
    pub const START_BYTE: &str = "start_byte";
    pub const END_BYTE: &str = "end_byte";
    pub const NAME_SPAN: &str = "name_span";
    pub const BODY_SPAN: &str = "body_span";
    pub const PARAMS_SPAN: &str = "params_span";
    pub const SIGNATURE: &str = "signature";
    pub const DOCSTRING: &str = "docstring";
    pub const IS_ASYNC: &str = "is_async";
    pub const IS_STATIC: &str = "is_static";
    pub const DECORATORS: &str = "decorators";
    pub const CONTENT_HASH: &str = "content_hash";
    pub const PARSE_QUALITY: &str = "parse_quality";
    pub const RETURN_TYPE: &str = "return_type";
}

/// Edge types mapped to Macrame edge assertions.
pub mod edge_type {
    pub const CONTAINS: &str = "contains";
    pub const CALLS: &str = "calls";
    pub const IMPORTS: &str = "imports";
    pub const EXTENDS: &str = "extends";
    pub const IMPLEMENTS: &str = "implements";
    pub const REFERENCES: &str = "references";
    pub const DECORATES: &str = "decorates";
    pub const INSTANTIATES: &str = "instantiates";
    pub const OVERRIDES: &str = "overrides";
}

/// Edge property keys stored in Macrame's JSON properties field.
pub mod edge_property {
    pub const CONFIDENCE: &str = "confidence";
    pub const RESOLUTION_METHOD: &str = "resolution_method";
    pub const LINE: &str = "line";
    pub const CALL_SITE_SPAN: &str = "call_site_span";
    pub const PROVENANCE: &str = "provenance";
    pub const SYNTHESIZED_BY: &str = "synthesizedBy";
}

/// Build annotation key-value pairs for a CodeRadar entity.
/// These are stored as Macrame concept annotations.
pub fn build_entity_annotations(unit: &ExtractedUnit, language: &str) -> Vec<(String, String)> {
    let mut ann = Vec::new();

    match unit {
        ExtractedUnit::Function(f) => {
            ann.push((annotation::KIND.into(), "function".into()));
            ann.push((annotation::LINE.into(), f.line.to_string()));
            ann.push((annotation::END_LINE.into(), f.exit_line.to_string()));
            ann.push((annotation::NAME_SPAN.into(), span_to_string(f.name_span)));
            ann.push((annotation::BODY_SPAN.into(), span_to_string(f.body_span)));
            ann.push((annotation::PARAMS_SPAN.into(), span_to_string(f.params_span)));
            ann.push((annotation::CONTENT_HASH.into(), format!("{:x}", f.body_hash)));
            ann.push((annotation::PARSE_QUALITY.into(), format!("{:?}", f.parse_quality)));
            if f.is_async {
                ann.push((annotation::IS_ASYNC.into(), "true".into()));
            }
            if !f.decorators.is_empty() {
                ann.push((annotation::DECORATORS.into(), f.decorators.join("\0")));
            }
            if let Some(ref dt) = f.docstring {
                ann.push((annotation::DOCSTRING.into(), dt.clone()));
            }
        }
        ExtractedUnit::Class(c) => {
            ann.push((annotation::KIND.into(), "class".into()));
            ann.push((annotation::LINE.into(), c.line.to_string()));
            ann.push((annotation::END_LINE.into(), c.exit_line.to_string()));
            ann.push((annotation::NAME_SPAN.into(), span_to_string(c.name_span)));
            ann.push((annotation::BODY_SPAN.into(), span_to_string(c.body_span)));
            ann.push((annotation::PARSE_QUALITY.into(), format!("{:?}", c.parse_quality)));
            if !c.decorators.is_empty() {
                ann.push((annotation::DECORATORS.into(), c.decorators.join("\0")));
            }
        }
        ExtractedUnit::Import(i) => {
            ann.push((annotation::KIND.into(), "import".into()));
            ann.push((annotation::LINE.into(), i.line.to_string()));
            ann.push((annotation::NAME_SPAN.into(), span_to_string(i.name_span)));
        }
        _ => {
            ann.push((annotation::KIND.into(), "other".into()));
        }
    }

    ann.push((annotation::LANGUAGE.into(), language.into()));
    ann.push((annotation::START_BYTE.into(), unit_byte_start(unit).to_string()));
    ann.push((annotation::END_BYTE.into(), unit_byte_end(unit).to_string()));

    ann
}

fn span_to_string(span: ByteSpan) -> String {
    format!("{}..{}", span.start, span.end)
}

fn unit_byte_start(unit: &ExtractedUnit) -> usize {
    match unit {
        ExtractedUnit::Function(f) => f.span.start,
        ExtractedUnit::Class(c) => c.span.start,
        ExtractedUnit::Import(i) => i.name_span.start,
        ExtractedUnit::Constant(c) => c.span.start,
        ExtractedUnit::TypeAlias(t) => t.span.start,
        ExtractedUnit::Field(f) => f.span.start,
        ExtractedUnit::Module(_) => 0,
    }
}

fn unit_byte_end(unit: &ExtractedUnit) -> usize {
    match unit {
        ExtractedUnit::Function(f) => f.span.end,
        ExtractedUnit::Class(c) => c.span.end,
        ExtractedUnit::Import(i) => i.name_span.end,
        ExtractedUnit::Constant(c) => c.span.end,
        ExtractedUnit::TypeAlias(t) => t.span.end,
        ExtractedUnit::Field(f) => f.span.end,
        ExtractedUnit::Module(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_function_annotations() {
        let f = ExtractedUnit::Function(ExtractedFunction {
            id: "test.py::foo".into(),
            name: "foo".into(),
            qualified_name: "foo".into(),
            parent_module: "test.py".into(),
            parent_class: None,
            parameters: vec![],
            return_type: None,
            calls: vec![],
            decorators: vec!["@staticmethod".into()],
            docstring: Some("Does stuff.".into()),
            kind: FunctionKind::StaticMethod,
            is_async: true,
            is_generator: false,
            line: 42,
            exit_line: 58,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            signature_hash: 0,
            body_hash: 12345,
            span: ByteSpan { start: 1000, end: 1500 },
            name_span: ByteSpan { start: 1004, end: 1007 },
            params_span: ByteSpan { start: 1008, end: 1020 },
            body_span: ByteSpan { start: 1030, end: 1490 },
            decorators_span: None,
        });

        let ann = build_entity_annotations(&f, "python");
        let map: std::collections::HashMap<_, _> = ann.into_iter().collect();

        assert_eq!(map.get("kind").map(|s| s.as_str()), Some("function"));
        assert_eq!(map.get("language").map(|s| s.as_str()), Some("python"));
        assert_eq!(map.get("is_async").map(|s| s.as_str()), Some("true"));
        assert!(map.get("decorators").map(|s| s.as_str()).unwrap_or("").contains("@staticmethod"));
        assert_eq!(map.get("docstring").map(|s| s.as_str()), Some("Does stuff."));
    }

    #[test]
    fn test_build_class_annotations() {
        let c = ExtractedUnit::Class(ExtractedClass {
            id: "test.py::MyClass".into(),
            name: "MyClass".into(),
            qualified_name: "MyClass".into(),
            parent_module: "test.py".into(),
            parent_class: None,
            bases: vec![],
            decorators: vec!["@dataclass".into()],
            docstring: None,
            fields: vec![],
            line: 10,
            exit_line: 30,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            span: ByteSpan { start: 200, end: 800 },
            name_span: ByteSpan { start: 206, end: 213 },
            body_span: ByteSpan { start: 220, end: 790 },
            decorators_span: None,
        });

        let ann = build_entity_annotations(&c, "python");
        let map: std::collections::HashMap<_, _> = ann.into_iter().collect();

        assert_eq!(map.get("kind").map(|s| s.as_str()), Some("class"));
        assert_eq!(map.get("line").map(|s| s.as_str()), Some("10"));
    }
}
