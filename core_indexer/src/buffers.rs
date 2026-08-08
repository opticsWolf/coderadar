// CodeRadar v3.5 — Flat-Buffer FFI Serialization (§8.1, Appendix A)
// One boundary crossing per file. Entities + edges encoded as fixed-layout rows
// with string data in a separate arena buffer.

use crate::types::*;

/// ABI version — bump on any wire format change.
pub const ABI_VERSION: u32 = 1;

// Row sizes (must match Python decoder)
pub const ENTITY_ROW_SIZE: usize = 132;
pub const EDGE_ROW_SIZE: usize = 56;
pub const REF_ROW_SIZE: usize = 48;
pub const META_SIZE: usize = 40;

/// Encoded extraction result for one file.
pub struct FlatBuffer {
    pub meta: Vec<u8>,
    pub entities: Vec<u8>,
    pub edges: Vec<u8>,
    pub arena: Vec<u8>,
}

/// EntityKind indices for the wire format.
#[repr(u8)]
pub enum WireEntityKind {
    Module = 0,
    Class = 1,
    Function = 2,
    Method = 3,
    Variable = 4,
    Import = 5,
    Constant = 6,
    TypeAlias = 7,
}

impl From<&ExtractedUnit> for WireEntityKind {
    fn from(unit: &ExtractedUnit) -> Self {
        match unit {
            ExtractedUnit::Module(_) => WireEntityKind::Module,
            ExtractedUnit::Class(_) => WireEntityKind::Class,
            ExtractedUnit::Function(f) => {
                if f.parent_class.is_some() {
                    WireEntityKind::Method
                } else {
                    WireEntityKind::Function
                }
            }
            ExtractedUnit::Import(_) => WireEntityKind::Import,
            ExtractedUnit::Constant(_) => WireEntityKind::Constant,
            ExtractedUnit::TypeAlias(_) => WireEntityKind::TypeAlias,
            ExtractedUnit::Field(_) => WireEntityKind::Variable,
        }
    }
}

/// Encode a set of extracted units into flat buffers.
/// Returns (meta, entities, edges, arena).
pub fn encode_extraction(units: &[ExtractedUnit], file_path: &str, duration_ms: f64) -> FlatBuffer {
    let mut arena = ArenaWriter::new();
    let mut entity_buf = Vec::new();

    // Encode entities
    for unit in units {
        encode_entity_row(unit, file_path, &mut entity_buf, &mut arena);
    }

    let entity_count = (entity_buf.len() / ENTITY_ROW_SIZE) as u32;
    let edge_count = 0u32; // edges encoded separately by resolution engine

    // Build meta buffer
    let mut meta = vec![0u8; META_SIZE];
    // bytes 0-3: ABI version
    meta[0..4].copy_from_slice(&ABI_VERSION.to_le_bytes());
    // bytes 4-7: entity count
    meta[4..8].copy_from_slice(&entity_count.to_le_bytes());
    // bytes 8-11: edge count
    meta[8..12].copy_from_slice(&edge_count.to_le_bytes());
    // bytes 12-15: arena length
    let arena_len = arena.buf.len() as u32;
    meta[12..16].copy_from_slice(&arena_len.to_le_bytes());
    // bytes 16-19: errors offset (NONE = no errors)
    meta[16..20].copy_from_slice(&0u32.to_le_bytes());
    // bytes 20-23: errors length
    meta[20..24].copy_from_slice(&0u32.to_le_bytes());
    // bytes 24-31: duration (f64)
    meta[24..32].copy_from_slice(&duration_ms.to_le_bytes());
    // bytes 32-39: reserved

    FlatBuffer {
        meta,
        entities: entity_buf,
        edges: Vec::new(),
        arena: arena.buf,
    }
}

fn encode_entity_row(
    unit: &ExtractedUnit,
    file_path: &str,
    buf: &mut Vec<u8>,
    arena: &mut ArenaWriter,
) {
    let start = buf.len();
    buf.resize(start + ENTITY_ROW_SIZE, 0);

    let kind: WireEntityKind = unit.into();
    buf[start] = kind as u8;
    // Language index at offset 1 — set by caller
    // Flags at offset 2–3
    // Lines at 4–11
    // Columns at 12–19

    // Write string fields into arena, store (offset, len) in row
    let name = arena.write(unit_name(unit));
    write_str_field(buf, start + 20, name);

    let qname = arena.write(&unit_qualified_name(unit));
    write_str_field(buf, start + 28, qname);

    let id = arena.write(&unit.entity_id());
    write_str_field(buf, start + 36, id);

    // Docstring, signature, return_type, decorators, parent_id: NONE for now
    write_str_field(buf, start + 44, ArenaRef::none());
    write_str_field(buf, start + 52, ArenaRef::none());
    write_str_field(buf, start + 60, ArenaRef::none());
    write_str_field(buf, start + 68, ArenaRef::none());
    write_str_field(buf, start + 76, ArenaRef::none());

    // Spans at 116-131
    let span = unit_span(unit);
    buf[start + 116..start + 120].copy_from_slice(&(span.start as u32).to_le_bytes());
    buf[start + 120..start + 124].copy_from_slice(&(span.end as u32).to_le_bytes());

    let name_span = unit_name_span(unit);
    buf[start + 124..start + 128].copy_from_slice(&(name_span.start as u32).to_le_bytes());
    buf[start + 128..start + 132].copy_from_slice(&(name_span.end as u32).to_le_bytes());
}

fn unit_name(unit: &ExtractedUnit) -> &str {
    match unit {
        ExtractedUnit::Module(m) => &m.name,
        ExtractedUnit::Class(c) => &c.name,
        ExtractedUnit::Function(f) => &f.name,
        ExtractedUnit::Import(_) => "",
        ExtractedUnit::Constant(c) => &c.name,
        ExtractedUnit::TypeAlias(t) => &t.name,
        ExtractedUnit::Field(f) => &f.name,
    }
}

fn unit_qualified_name(unit: &ExtractedUnit) -> String {
    match unit {
        ExtractedUnit::Class(c) => c.qualified_name.clone(),
        ExtractedUnit::Function(f) => f.qualified_name.clone(),
        _ => String::new(),
    }
}

fn unit_span(unit: &ExtractedUnit) -> ByteSpan {
    match unit {
        ExtractedUnit::Class(c) => c.span,
        ExtractedUnit::Function(f) => f.span,
        ExtractedUnit::Constant(c) => c.span,
        ExtractedUnit::TypeAlias(t) => t.span,
        ExtractedUnit::Import(i) => i.name_span,
        ExtractedUnit::Field(f) => f.span,
        ExtractedUnit::Module(_) => ByteSpan { start: 0, end: 0 },
    }
}

fn unit_name_span(unit: &ExtractedUnit) -> ByteSpan {
    match unit {
        ExtractedUnit::Class(c) => c.name_span,
        ExtractedUnit::Function(f) => f.name_span,
        ExtractedUnit::Constant(c) => c.name_span,
        ExtractedUnit::TypeAlias(t) => t.name_span,
        ExtractedUnit::Import(i) => i.name_span,
        ExtractedUnit::Field(f) => f.name_span,
        ExtractedUnit::Module(_) => ByteSpan { start: 0, end: 0 },
    }
}

fn write_str_field(buf: &mut [u8], offset: usize, r: ArenaRef) {
    buf[offset..offset + 4].copy_from_slice(&r.offset.to_le_bytes());
    buf[offset + 4..offset + 8].copy_from_slice(&r.length.to_le_bytes());
}

// ── Arena Writer ────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
struct ArenaRef {
    offset: u32,
    length: u32,
}

impl ArenaRef {
    fn none() -> Self {
        Self { offset: u32::MAX, length: 0 }
    }
}

struct ArenaWriter {
    buf: Vec<u8>,
    strings: std::collections::HashMap<String, ArenaRef>,
}

impl ArenaWriter {
    fn new() -> Self {
        Self { buf: Vec::new(), strings: std::collections::HashMap::new() }
    }

    fn write(&mut self, s: &str) -> ArenaRef {
        if s.is_empty() {
            return ArenaRef::none();
        }
        if let Some(existing) = self.strings.get(s) {
            return *existing;
        }
        let offset = self.buf.len() as u32;
        let bytes = s.as_bytes();
        self.buf.extend_from_slice(bytes);
        let r = ArenaRef { offset, length: bytes.len() as u32 };
        self.strings.insert(s.to_string(), r);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_empty() {
        let fb = encode_extraction(&[], "test.py", 0.0);
        assert_eq!(fb.meta.len(), META_SIZE);
        assert_eq!(fb.entities.len(), 0);
        assert_eq!(fb.edges.len(), 0);
    }

    #[test]
    fn test_abi_version_in_meta() {
        let fb = encode_extraction(&[], "test.py", 0.0);
        let abi = u32::from_le_bytes(fb.meta[0..4].try_into().unwrap());
        assert_eq!(abi, ABI_VERSION);
    }

    #[test]
    fn test_encode_function() {
        let f = ExtractedUnit::Function(ExtractedFunction {
            id: "test.py::foo".into(),
            name: "foo".into(),
            qualified_name: "foo".into(),
            parent_module: "test.py".into(),
            parent_class: None,
            parameters: vec![],
            return_type: None,
            calls: vec![],
            decorators: vec![],
            docstring: None,
            kind: FunctionKind::Free,
            is_async: false,
            is_generator: false,
            line: 10,
            exit_line: 15,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            signature_hash: 0,
            body_hash: 0,
            span: ByteSpan { start: 100, end: 200 },
            name_span: ByteSpan { start: 104, end: 107 },
            params_span: ByteSpan { start: 0, end: 0 },
            body_span: ByteSpan { start: 0, end: 0 },
            decorators_span: None,
        });

        let fb = encode_extraction(&[f], "test.py", 1.5);
        assert_eq!(fb.entities.len(), ENTITY_ROW_SIZE);
        assert!(!fb.arena.is_empty(), "Arena should contain name/qualified_name/id strings");
    }
}
