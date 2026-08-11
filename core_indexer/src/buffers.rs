// CodeRadar v3.6 — Flat-Buffer FFI Serialization (§8.1, Appendix A)
//
// Flat-buffer encoding pattern adapted from CodeGraph's buffers.rs
// (MIT/Apache-2.0, opticsWolf/codegraph). One boundary crossing per file.
// Entities, edges, and refs encoded as fixed-layout rows with string data in
// a separate UTF-8 arena. `OFFSET == NONE (0xFFFF_FFFF)` means "field absent".
//
// Layout (v1):
//
// meta (36 bytes):                        ENTITY_ROW (132 bytes):
//   0   u8   ABI_VERSION                   0   u8   EntityKind index
//   1   [3]  pad                           1   u8   language index
//   4   u32  entity count                  2   u16  bool flags (bit pairs)
//   8   u32  edge count                    4   u32  start_line
//   12  u32  ref count                     8   u32  end_line
//   16  u32  arena byte length             12  u32  start_byte
//   20  u32  errors offset (NONE=none)     16  u32  end_byte
//   24  u32  errors length                 20  str  name
//   28  f64  duration_ms                   28  str  qualified_name
//                                          36  str  id
// EDGE_ROW (56 bytes):                     44  str  docstring
//   0   u32  source entity index           52  str  signature
//   4   u32  target entity index           60  str  return_type
//   8   u8   EdgeKind index                68  str  decorators (NUL-joined)
//   9   u8   provenance                    76  str  parent_id
//   10  [2]  pad                           84  str  extra_json
//   12  u32  line                          92  str  file_path
//   16  u32  column                        100 str  content_hash
//   20  str  metadata_json                 108 u32  visibility
//   28  str  source_id_str                 112 u32  metrics slot (reserved)
//   36  str  target_id_str                 116 u32  span_start
//   44  str  edge_properties_json           120 u32  span_end
//                                          124 u32  name_span_start
// REF_ROW (48 bytes):                      128 u32  name_span_end
//   0   u32  from_entity index
//   4   u8   ReferenceKind index
//   5   u8   flags
//   6   [2]  pad
//   8   u32  line
//   12  u32  column
//   16  str  reference_name
//   24  str  candidates (NUL-joined)
//   32  str  from_entity_id_str
//   40  str  context_json

use crate::types::*;

pub const KERNEL_ABI_VERSION: u8 = 1;
pub const NONE: u32 = 0xFFFF_FFFF;

pub const META_SIZE: usize = 36;
pub const ENTITY_ROW_SIZE: usize = 132;
pub const EDGE_ROW_SIZE: usize = 52;
pub const REF_ROW_SIZE: usize = 48;

/// Copy of ENTITY_KINDS — order is the wire contract.
pub const ENTITY_KINDS: [&str; 7] = [
    "module", "class", "function", "method", "import", "constant", "type_alias",
];

pub fn entity_kind_index(kind: &str) -> Option<u8> {
    ENTITY_KINDS.iter().position(|k| *k == kind).map(|i| i as u8)
}

/// Copy of EDGE_KINDS — order is the wire contract.
pub const EDGE_KINDS: [&str; 10] = [
    "contains", "calls", "imports", "extends", "implements",
    "references", "decorates", "instantiates", "overrides", "exports",
];

pub fn edge_kind_index(kind: &str) -> Option<u8> {
    EDGE_KINDS.iter().position(|k| *k == kind).map(|i| i as u8)
}

// ── Tri-State Bool Flags ────────────────────────────────────────────────────

/// Tri-state booleans packed as (present, value) bit pairs.
/// Each flag occupies two bits: bit 0 = present, bit 1 = value.
/// Absent (00) = field not applicable, Present-False (01), Present-True (11).
#[derive(Default, Clone, Copy)]
pub struct BoolFlags(pub u16);

impl BoolFlags {
    pub fn set(&mut self, pair: u16, value: bool) {
        self.0 |= 1 << (pair * 2); // present
        if value {
            self.0 |= 1 << (pair * 2 + 1);
        }
    }

    pub fn is_set(&self, pair: u16) -> bool {
        (self.0 >> (pair * 2)) & 1 != 0
    }

    pub fn value(&self, pair: u16) -> Option<bool> {
        if self.is_set(pair) {
            Some((self.0 >> (pair * 2 + 1)) & 1 != 0)
        } else {
            None
        }
    }
}

// Flag indices for entity rows
pub const FLAG_IS_EXPORTED: u16 = 0;
pub const FLAG_IS_ASYNC: u16 = 1;
pub const FLAG_IS_STATIC: u16 = 2;
pub const FLAG_IS_ABSTRACT: u16 = 3;
pub const FLAG_IS_GENERATOR: u16 = 4;
pub const FLAG_IS_TYPE_CHECKING: u16 = 5;

// ── Arena ───────────────────────────────────────────────────────────────────

pub type StrRef = (u32, u32);

/// UTF-8 string arena. Strings appended verbatim; no dedup (per-file buffers
/// are transient — intern later if profiling says so).
#[derive(Default)]
pub struct Arena {
    buf: Vec<u8>,
}

impl Arena {
    pub fn put(&mut self, s: &str) -> StrRef {
        if s.is_empty() {
            return NONE_STR;
        }
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(s.as_bytes());
        (off, s.len() as u32)
    }

    pub fn put_opt(&mut self, s: Option<&str>) -> StrRef {
        match s {
            Some(s) => self.put(s),
            None => NONE_STR,
        }
    }

    /// NUL-joined list; absent when the list is empty.
    pub fn put_list(&mut self, items: &[String]) -> StrRef {
        if items.is_empty() {
            return NONE_STR;
        }
        let joined = items.join("\0");
        self.put(&joined)
    }

    pub fn len(&self) -> u32 {
        self.buf.len() as u32
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

pub const NONE_STR: StrRef = (NONE, 0);

// ── Row Structs ─────────────────────────────────────────────────────────────

pub struct EntityRow {
    pub kind: u8,
    pub language: u8,
    pub flags: BoolFlags,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub name: StrRef,
    pub qualified_name: StrRef,
    pub id: StrRef,
    pub docstring: StrRef,
    pub signature: StrRef,
    pub return_type: StrRef,
    pub decorators: StrRef,
    pub parent_id: StrRef,
    pub extra_json: StrRef,
    pub file_path: StrRef,
    pub content_hash: StrRef,
    pub visibility: u32,
    pub metrics: u32,
    pub span_start: u32,
    pub span_end: u32,
    pub name_span_start: u32,
    pub name_span_end: u32,
}

pub struct EdgeRow {
    pub source_idx: u32,
    pub target_idx: u32,
    pub kind: u8,
    pub provenance: u8,
    pub line: u32,
    pub column: u32,
    pub metadata_json: StrRef,
    pub source_id_str: StrRef,
    pub target_id_str: StrRef,
    pub properties_json: StrRef,
}

pub struct RefRow {
    pub from_idx: u32,
    pub kind: u8,
    pub flags: u8,
    pub line: u32,
    pub column: u32,
    pub reference_name: StrRef,
    pub candidates: StrRef,
    pub from_id_str: StrRef,
    pub context_json: StrRef,
}

// ── Tables ──────────────────────────────────────────────────────────────────

fn push_str_ref(buf: &mut Vec<u8>, r: StrRef) {
    buf.extend_from_slice(&r.0.to_le_bytes());
    buf.extend_from_slice(&r.1.to_le_bytes());
}

pub struct Tables {
    pub entities: Vec<u8>,
    pub edges: Vec<u8>,
    pub refs: Vec<u8>,
    pub entity_count: u32,
    pub edge_count: u32,
    pub ref_count: u32,
}

impl Default for Tables {
    fn default() -> Self {
        Tables {
            entities: Vec::with_capacity(ENTITY_ROW_SIZE * 64),
            edges: Vec::with_capacity(EDGE_ROW_SIZE * 64),
            refs: Vec::with_capacity(REF_ROW_SIZE * 64),
            entity_count: 0,
            edge_count: 0,
            ref_count: 0,
        }
    }
}

impl Tables {
    pub fn push_entity(&mut self, r: &EntityRow) -> u32 {
        let buf = &mut self.entities;
        buf.push(r.kind);
        buf.push(r.language);
        buf.extend_from_slice(&r.flags.0.to_le_bytes());
        buf.extend_from_slice(&r.start_line.to_le_bytes());
        buf.extend_from_slice(&r.end_line.to_le_bytes());
        buf.extend_from_slice(&r.start_byte.to_le_bytes());
        buf.extend_from_slice(&r.end_byte.to_le_bytes());
        push_str_ref(buf, r.name);
        push_str_ref(buf, r.qualified_name);
        push_str_ref(buf, r.id);
        push_str_ref(buf, r.docstring);
        push_str_ref(buf, r.signature);
        push_str_ref(buf, r.return_type);
        push_str_ref(buf, r.decorators);
        push_str_ref(buf, r.parent_id);
        push_str_ref(buf, r.extra_json);
        push_str_ref(buf, r.file_path);
        push_str_ref(buf, r.content_hash);
        buf.extend_from_slice(&r.visibility.to_le_bytes());
        buf.extend_from_slice(&r.metrics.to_le_bytes());
        buf.extend_from_slice(&r.span_start.to_le_bytes());
        buf.extend_from_slice(&r.span_end.to_le_bytes());
        buf.extend_from_slice(&r.name_span_start.to_le_bytes());
        buf.extend_from_slice(&r.name_span_end.to_le_bytes());
        let idx = self.entity_count;
        self.entity_count += 1;
        idx
    }

    pub fn push_edge(&mut self, r: &EdgeRow) {
        let buf = &mut self.edges;
        buf.extend_from_slice(&r.source_idx.to_le_bytes());
        buf.extend_from_slice(&r.target_idx.to_le_bytes());
        buf.push(r.kind);
        buf.push(r.provenance);
        buf.extend_from_slice(&0u16.to_le_bytes()); // pad
        buf.extend_from_slice(&r.line.to_le_bytes());
        buf.extend_from_slice(&r.column.to_le_bytes());
        push_str_ref(buf, r.metadata_json);
        push_str_ref(buf, r.source_id_str);
        push_str_ref(buf, r.target_id_str);
        push_str_ref(buf, r.properties_json);
        self.edge_count += 1;
    }

    pub fn push_ref(&mut self, r: &RefRow) {
        let buf = &mut self.refs;
        buf.extend_from_slice(&r.from_idx.to_le_bytes());
        buf.push(r.kind);
        buf.push(r.flags);
        buf.extend_from_slice(&[0u8; 2]); // pad
        buf.extend_from_slice(&r.line.to_le_bytes());
        buf.extend_from_slice(&r.column.to_le_bytes());
        push_str_ref(buf, r.reference_name);
        push_str_ref(buf, r.candidates);
        push_str_ref(buf, r.from_id_str);
        push_str_ref(buf, r.context_json);
        self.ref_count += 1;
    }
}

// ── Meta + Emit ─────────────────────────────────────────────────────────────

pub fn build_meta(t: &Tables, arena_len: u32, errors_json: StrRef, duration_ms: f64) -> Vec<u8> {
    let mut m = Vec::with_capacity(META_SIZE);
    m.push(KERNEL_ABI_VERSION);
    m.extend_from_slice(&[0u8; 3]); // pad
    m.extend_from_slice(&t.entity_count.to_le_bytes());
    m.extend_from_slice(&t.edge_count.to_le_bytes());
    m.extend_from_slice(&t.ref_count.to_le_bytes());
    m.extend_from_slice(&arena_len.to_le_bytes());
    m.extend_from_slice(&errors_json.0.to_le_bytes());
    m.extend_from_slice(&errors_json.1.to_le_bytes());
    m.extend_from_slice(&duration_ms.to_le_bytes());
    debug_assert_eq!(m.len(), META_SIZE);
    m
}

/// One file's encoded tables, ready to hand across the Python boundary.
pub struct EmitOut {
    pub meta: Vec<u8>,
    pub entities: Vec<u8>,
    pub edges: Vec<u8>,
    pub refs: Vec<u8>,
    pub arena: Vec<u8>,
}

/// Encode a set of extracted units into flat buffers.
pub fn encode_extraction(
    units: &[ExtractedUnit],
    arena: &mut Arena,
    file_path: &str,
    duration_ms: f64,
) -> EmitOut {
    let mut t = Tables::default();

    for unit in units {
        encode_entity_row(unit, file_path, arena, &mut t);
    }

    EmitOut {
        meta: build_meta(&t, arena.len(), NONE_STR, duration_ms),
        entities: t.entities,
        edges: t.edges,
        refs: t.refs,
        arena: arena.buf.clone(),
    }
}

fn encode_entity_row(
    unit: &ExtractedUnit,
    file_path: &str,
    arena: &mut Arena,
    t: &mut Tables,
) {
    let mut flags = BoolFlags::default();
    let mut kind: u8 = 0;
    let (name, qname, id_str, line, exit_line, span, name_span, doc, sig, rt, decs, parent) = entity_fields(unit, &mut flags);

    kind = entity_kind_idx(unit);

    let row = EntityRow {
        kind,
        language: 0, // filled by caller from language index
        flags,
        start_line: line as u32,
        end_line: exit_line as u32,
        start_byte: span.start as u32,
        end_byte: span.end as u32,
        name: arena.put(&name),
        qualified_name: arena.put_opt(qname.as_deref()),
        id: arena.put(&id_str),
        docstring: arena.put_opt(doc.as_deref()),
        signature: arena.put_opt(sig.as_deref()),
        return_type: arena.put_opt(rt.as_deref()),
        decorators: arena.put_list(&decs),
        parent_id: arena.put_opt(parent.as_deref()),
        extra_json: NONE_STR,
        file_path: arena.put(file_path),
        content_hash: NONE_STR,
        visibility: 0,
        metrics: 0,
        span_start: span.start as u32,
        span_end: span.end as u32,
        name_span_start: name_span.start as u32,
        name_span_end: name_span.end as u32,
    };

    t.push_entity(&row);
}

fn entity_kind_idx(unit: &ExtractedUnit) -> u8 {
    match unit {
        ExtractedUnit::Module(_) => 0,
        ExtractedUnit::Class(_) => 1,
        ExtractedUnit::Function(f) => {
            if f.parent_class.is_some() { 3 } else { 2 }
        }
        ExtractedUnit::Import(_) => 4,
        ExtractedUnit::Constant(_) => 5,
        ExtractedUnit::TypeAlias(_) => 6,
        ExtractedUnit::Field(_) => 1, // mapped as class member
    }
}

fn entity_fields(unit: &ExtractedUnit, flags: &mut BoolFlags) -> (
    String, Option<String>, String, usize, usize,
    ByteSpan, ByteSpan, Option<String>, Option<String>,
    Option<String>, Vec<String>, Option<String>,
) {
    match unit {
        ExtractedUnit::Function(f) => {
            if f.is_async { flags.set(FLAG_IS_ASYNC, true); }
            if f.is_generator { flags.set(FLAG_IS_GENERATOR, true); }
            if f.kind == FunctionKind::StaticMethod { flags.set(FLAG_IS_STATIC, true); }
            if f.is_type_checking_only { flags.set(FLAG_IS_TYPE_CHECKING, true); }
            (
                f.name.clone(), Some(f.qualified_name.clone()), f.id.clone(),
                f.line, f.exit_line, f.span, f.name_span,
                f.docstring.clone(), None, f.return_type.clone(),
                f.decorators.clone(), f.parent_class.clone(),
            )
        }
        ExtractedUnit::Class(c) => {
            if c.is_type_checking_only { flags.set(FLAG_IS_TYPE_CHECKING, true); }
            (
                c.name.clone(), Some(c.qualified_name.clone()), c.id.clone(),
                c.line, c.exit_line, c.span, c.name_span,
                c.docstring.clone(), None, None,
                c.decorators.clone(), c.parent_class.clone(),
            )
        }
        ExtractedUnit::Import(i) => {
            (
                i.raw.clone(), None, i.id.clone(),
                i.line, i.line, i.name_span, i.name_span,
                None, None, None, vec![], None,
            )
        }
        ExtractedUnit::Constant(c) => {
            (
                c.name.clone(), None, c.id.clone(),
                0, 0, c.span, c.name_span,
                None, None, None, vec![], None,
            )
        }
        ExtractedUnit::TypeAlias(t) => {
            (
                t.name.clone(), None, t.id.clone(),
                0, 0, t.span, t.name_span,
                None, None, None, vec![], None,
            )
        }
        ExtractedUnit::Field(f) => {
            if f.is_class_var { flags.set(FLAG_IS_STATIC, true); }
            (
                f.name.clone(), None, String::new(),
                0, 0, f.span, f.name_span,
                None, None, f.annotation.clone(), vec![], None,
            )
        }
        ExtractedUnit::Module(m) => {
            (
                m.name.clone(), None, m.id.clone(),
                0, 0, ByteSpan { start: 0, end: 0 }, ByteSpan { start: 0, end: 0 },
                None, None, None, vec![], None,
            )
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_sizes_match_constants() {
        let mut t = Tables::default();
        let mut a = Arena::default();
        let name = a.put("x");
        t.push_entity(&EntityRow {
            kind: 0, language: 0, flags: BoolFlags::default(),
            start_line: 1, end_line: 1, start_byte: 0, end_byte: 0,
            name, qualified_name: name, id: name,
            docstring: NONE_STR, signature: NONE_STR, return_type: NONE_STR,
            decorators: NONE_STR, parent_id: NONE_STR, extra_json: NONE_STR,
            file_path: name, content_hash: NONE_STR,
            visibility: 0, metrics: 0,
            span_start: 0, span_end: 10, name_span_start: 0, name_span_end: 5,
        });
        assert_eq!(t.entities.len(), ENTITY_ROW_SIZE,
            "ENTITY_ROW_SIZE constant ({ENTITY_ROW_SIZE}) doesn't match actual row size ({})",
            t.entities.len());

        t.push_edge(&EdgeRow {
            source_idx: 0, target_idx: 0, kind: 0, provenance: 0,
            line: NONE, column: NONE,
            metadata_json: NONE_STR, source_id_str: NONE_STR,
            target_id_str: NONE_STR, properties_json: NONE_STR,
        });
        assert_eq!(t.edges.len(), EDGE_ROW_SIZE,
            "EDGE_ROW_SIZE constant ({EDGE_ROW_SIZE}) doesn't match actual row size ({})",
            t.edges.len());

        t.push_ref(&RefRow {
            from_idx: 0, kind: 0, flags: 0, line: 1, column: 0,
            reference_name: name, candidates: NONE_STR,
            from_id_str: NONE_STR, context_json: NONE_STR,
        });
        assert_eq!(t.refs.len(), REF_ROW_SIZE,
            "REF_ROW_SIZE constant ({REF_ROW_SIZE}) doesn't match actual row size ({})",
            t.refs.len());

        let meta = build_meta(&t, a.len(), NONE_STR, 0.0);
        assert_eq!(meta.len(), META_SIZE);
    }

    #[test]
    fn bool_flags_set_and_read() {
        let mut f = BoolFlags::default();
        assert_eq!(f.value(FLAG_IS_ASYNC), None); // absent
        f.set(FLAG_IS_ASYNC, true);
        assert_eq!(f.value(FLAG_IS_ASYNC), Some(true));
        f.set(FLAG_IS_STATIC, false);
        assert_eq!(f.value(FLAG_IS_STATIC), Some(false));
    }

    #[test]
    fn encode_empty_extraction() {
        let mut arena = Arena::default();
        let out = encode_extraction(&[], &mut arena, "test.py", 0.0);
        assert_eq!(out.entities.len(), 0);
        assert_eq!(out.edges.len(), 0);
        assert_eq!(out.meta.len(), META_SIZE);
    }

    #[test]
    fn encode_function() {
        let f = ExtractedUnit::Function(ExtractedFunction {
            id: "test.py::foo".into(),
            name: "foo".into(),
            qualified_name: "foo".into(),
            parent_module: "test.py".into(),
            parent_class: None,
            parameters: vec![],
            return_type: Some("int".into()),
            calls: vec![],
            decorators: vec![],
            docstring: Some("Does stuff.".into()),
            kind: FunctionKind::Free,
            is_async: true,
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

        let mut arena = Arena::default();
        let out = encode_extraction(&[f], &mut arena, "test.py", 1.5);
        assert_eq!(out.entities.len(), ENTITY_ROW_SIZE);
        assert!(!out.arena.is_empty());
    }

    #[test]
    fn entity_kind_indices_are_stable() {
        assert_eq!(entity_kind_index("module"), Some(0));
        assert_eq!(entity_kind_index("class"), Some(1));
        assert_eq!(entity_kind_index("function"), Some(2));
        assert_eq!(entity_kind_index("method"), Some(3));
        assert_eq!(entity_kind_index("import"), Some(4));
        assert_eq!(entity_kind_index("constant"), Some(5));
        assert_eq!(entity_kind_index("type_alias"), Some(6));
    }

    #[test]
    fn edge_kind_indices_are_stable() {
        assert_eq!(edge_kind_index("contains"), Some(0));
        assert_eq!(edge_kind_index("calls"), Some(1));
        assert_eq!(edge_kind_index("imports"), Some(2));
        assert_eq!(edge_kind_index("extends"), Some(3));
        assert_eq!(edge_kind_index("implements"), Some(4));
    }
}
