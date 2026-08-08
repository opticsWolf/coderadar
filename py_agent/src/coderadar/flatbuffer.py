"""CodeRadar v3.5 — Flat-Buffer Decoder (§8.1, Appendix A)

Decodes the fixed-layout row format produced by core_indexer/src/buffers.rs.
One boundary crossing per file — entities, edges, and refs as fixed rows with
string data in a separate UTF-8 arena.

Wire contract (ABI_VERSION 1):
  Meta:     36 bytes
  Entity:  132 bytes each
  Edge:     52 bytes each
  Ref:      48 bytes each
  Arena:    UTF-8 bytes (variable length)

Entity Kind wire indices (stable):
  0: module, 1: class, 2: function, 3: method, 4: import, 5: constant, 6: type_alias

Edge Kind wire indices (stable):
  0: contains, 1: calls, 2: imports, 3: extends, 4: implements,
  5: references, 6: decorates, 7: instantiates, 8: overrides, 9: exports
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

# ── Constants (must match buffers.rs) ───────────────────────────────────────

KERNEL_ABI_VERSION: int = 1
NONE: int = 0xFFFF_FFFF

META_SIZE: int = 36
ENTITY_ROW_SIZE: int = 132
EDGE_ROW_SIZE: int = 52
REF_ROW_SIZE: int = 48

ENTITY_KINDS: List[str] = [
    "module", "class", "function", "method", "import", "constant", "type_alias",
]

EDGE_KINDS: List[str] = [
    "contains", "calls", "imports", "extends", "implements",
    "references", "decorates", "instantiates", "overrides", "exports",
]

FLAG_IS_EXPORTED: int = 0
FLAG_IS_ASYNC: int = 1
FLAG_IS_STATIC: int = 2
FLAG_IS_ABSTRACT: int = 3
FLAG_IS_GENERATOR: int = 4
FLAG_IS_TYPE_CHECKING: int = 5


# ── Decoded Types ────────────────────────────────────────────────────────────

@dataclass
class DecodedEntity:
    """A single entity decoded from the flat buffer."""
    kind: str
    language: int
    flags: Dict[str, Optional[bool]]
    start_line: int
    end_line: int
    start_byte: int
    end_byte: int
    name: str
    qualified_name: Optional[str]
    id: str
    docstring: Optional[str]
    signature: Optional[str]
    return_type: Optional[str]
    decorators: List[str]
    parent_id: Optional[str]
    file_path: str
    visibility: int
    span: Tuple[int, int]
    name_span: Tuple[int, int]


@dataclass
class DecodedEdge:
    """A single edge decoded from the flat buffer."""
    source_idx: int
    target_idx: int
    kind: str
    provenance: int
    line: int
    column: int
    metadata: dict
    source_id: Optional[str]
    target_id: Optional[str]
    properties: dict


@dataclass
class DecodedRef:
    """A single unresolved reference decoded from the flat buffer."""
    from_idx: int
    kind: int
    flags: int
    line: int
    column: int
    reference_name: str
    candidates: List[str]
    from_id: Optional[str]
    context: dict


@dataclass
class FlatBuffers:
    """Complete decoded flat-buffer payload for a single file extraction."""
    abi_version: int
    file_path: str
    entities: List[DecodedEntity]
    edges: List[DecodedEdge]
    refs: List[DecodedRef]
    errors_json: Optional[str]
    duration_ms: float


# ── BoolFlags Decoder ───────────────────────────────────────────────────────

def decode_bool_flags(flags: int) -> Dict[str, Optional[bool]]:
    """Decode the tri-state bool flags bit-pair encoding.

    Each flag occupies 2 bits: bit 0 = present, bit 1 = value.
    Absent (00) → None, Present-False (01) → False, Present-True (11) → True.
    10 is reserved but treated as Present-True.

    Returns a dict mapping flag names to Optional[bool].
    """
    NAMES = {
        FLAG_IS_EXPORTED: "is_exported",
        FLAG_IS_ASYNC: "is_async",
        FLAG_IS_STATIC: "is_static",
        FLAG_IS_ABSTRACT: "is_abstract",
        FLAG_IS_GENERATOR: "is_generator",
        FLAG_IS_TYPE_CHECKING: "is_type_checking",
    }
    result: Dict[str, Optional[bool]] = {}
    for pair_idx, name in NAMES.items():
        present = (flags >> (pair_idx * 2)) & 1
        value = (flags >> (pair_idx * 2 + 1)) & 1
        if present:
            result[name] = value != 0
        else:
            result[name] = None
    return result


# ── Arena String Extraction ─────────────────────────────────────────────────

def _read_arena_str(arena: bytes, offset: int, length: int) -> Optional[str]:
    """Read a string from the arena at a given offset and length.

    Returns None if offset is NONE or length is 0.
    """
    if offset == NONE or length == 0:
        return None
    try:
        return arena[offset:offset + length].decode("utf-8")
    except (IndexError, UnicodeDecodeError):
        return None


def _read_arena_list(arena: bytes, offset: int, length: int) -> List[str]:
    """Read a NUL-joined list of strings from the arena."""
    s = _read_arena_str(arena, offset, length)
    if not s:
        return []
    return [part for part in s.split("\0") if part]


def _read_arena_json(arena: bytes, offset: int, length: int) -> dict:
    """Read a JSON-encoded field from the arena. Returns {} on failure."""
    import json
    s = _read_arena_str(arena, offset, length)
    if not s:
        return {}
    try:
        return json.loads(s)
    except json.JSONDecodeError:
        return {}


# ── Row Decoders ────────────────────────────────────────────────────────────

def _decode_entity_row(data: bytes, arena: bytes, offset: int) -> DecodedEntity:
    """Decode a single 132-byte entity row."""
    pos = offset
    kind_idx = data[pos]; pos += 1
    language = data[pos]; pos += 1
    flags_raw = struct.unpack_from("<H", data, pos)[0]; pos += 2
    start_line = struct.unpack_from("<I", data, pos)[0]; pos += 4
    end_line = struct.unpack_from("<I", data, pos)[0]; pos += 4
    start_byte = struct.unpack_from("<I", data, pos)[0]; pos += 4
    end_byte = struct.unpack_from("<I", data, pos)[0]; pos += 4

    # Each StrRef is (offset: u32, length: u32)
    def _ref() -> Tuple[int, int]:
        nonlocal pos
        off = struct.unpack_from("<I", data, pos)[0]
        ln = struct.unpack_from("<I", data, pos + 4)[0]
        pos += 8
        return off, ln

    name_off, name_len = _ref()
    qname_off, qname_len = _ref()
    id_off, id_len = _ref()
    doc_off, doc_len = _ref()
    sig_off, sig_len = _ref()
    ret_off, ret_len = _ref()
    dec_off, dec_len = _ref()
    parent_off, parent_len = _ref()
    extra_off, extra_len = _ref()
    file_off, file_len = _ref()
    hash_off, hash_len = _ref()

    visibility = struct.unpack_from("<I", data, pos)[0]; pos += 4
    metrics = struct.unpack_from("<I", data, pos)[0]; pos += 4
    span_start = struct.unpack_from("<I", data, pos)[0]; pos += 4
    span_end = struct.unpack_from("<I", data, pos)[0]; pos += 4
    name_span_start = struct.unpack_from("<I", data, pos)[0]; pos += 4
    name_span_end = struct.unpack_from("<I", data, pos)[0]; pos += 4

    return DecodedEntity(
        kind=ENTITY_KINDS[kind_idx] if kind_idx < len(ENTITY_KINDS) else "unknown",
        language=language,
        flags=decode_bool_flags(flags_raw),
        start_line=start_line,
        end_line=end_line,
        start_byte=start_byte,
        end_byte=end_byte,
        name=_read_arena_str(arena, name_off, name_len) or "",
        qualified_name=_read_arena_str(arena, qname_off, qname_len),
        id=_read_arena_str(arena, id_off, id_len) or "",
        docstring=_read_arena_str(arena, doc_off, doc_len),
        signature=_read_arena_str(arena, sig_off, sig_len),
        return_type=_read_arena_str(arena, ret_off, ret_len),
        decorators=_read_arena_list(arena, dec_off, dec_len),
        parent_id=_read_arena_str(arena, parent_off, parent_len),
        file_path=_read_arena_str(arena, file_off, file_len) or "",
        visibility=visibility,
        span=(span_start, span_end),
        name_span=(name_span_start, name_span_end),
    )


def _decode_edge_rows(data: bytes, arena: bytes, count: int) -> List[DecodedEdge]:
    """Decode all 52-byte edge rows."""
    edges = []
    pos = 0
    for _ in range(count):
        source_idx = struct.unpack_from("<I", data, pos)[0]; pos += 4
        target_idx = struct.unpack_from("<I", data, pos)[0]; pos += 4
        kind_idx = data[pos]; pos += 1
        provenance = data[pos]; pos += 1
        pos += 2  # padding

        line = struct.unpack_from("<I", data, pos)[0]; pos += 4
        column = struct.unpack_from("<I", data, pos)[0]; pos += 4

        def _ref() -> Tuple[int, int]:
            nonlocal pos
            off = struct.unpack_from("<I", data, pos)[0]
            ln = struct.unpack_from("<I", data, pos + 4)[0]
            pos += 8
            return off, ln

        meta_off, meta_len = _ref()
        src_id_off, src_id_len = _ref()
        tgt_id_off, tgt_id_len = _ref()
        props_off, props_len = _ref()

        edges.append(DecodedEdge(
            source_idx=source_idx,
            target_idx=target_idx,
            kind=EDGE_KINDS[kind_idx] if kind_idx < len(EDGE_KINDS) else "unknown",
            provenance=provenance,
            line=line if line != NONE else 0,
            column=column if column != NONE else 0,
            metadata=_read_arena_json(arena, meta_off, meta_len),
            source_id=_read_arena_str(arena, src_id_off, src_id_len),
            target_id=_read_arena_str(arena, tgt_id_off, tgt_id_len),
            properties=_read_arena_json(arena, props_off, props_len),
        ))
    return edges


def _decode_ref_rows(data: bytes, arena: bytes, count: int) -> List[DecodedRef]:
    """Decode all 48-byte ref rows."""
    refs = []
    pos = 0
    for _ in range(count):
        from_idx = struct.unpack_from("<I", data, pos)[0]; pos += 4
        kind = data[pos]; pos += 1
        flags = data[pos]; pos += 1
        pos += 2  # padding

        line = struct.unpack_from("<I", data, pos)[0]; pos += 4
        column = struct.unpack_from("<I", data, pos)[0]; pos += 4

        def _ref() -> Tuple[int, int]:
            nonlocal pos
            off = struct.unpack_from("<I", data, pos)[0]
            ln = struct.unpack_from("<I", data, pos + 4)[0]
            pos += 8
            return off, ln

        ref_name_off, ref_name_len = _ref()
        candidates_off, candidates_len = _ref()
        from_id_off, from_id_len = _ref()
        ctx_off, ctx_len = _ref()

        refs.append(DecodedRef(
            from_idx=from_idx,
            kind=kind,
            flags=flags,
            line=line if line != NONE else 0,
            column=column if column != NONE else 0,
            reference_name=_read_arena_str(arena, ref_name_off, ref_name_len) or "",
            candidates=_read_arena_list(arena, candidates_off, candidates_len),
            from_id=_read_arena_str(arena, from_id_off, from_id_len),
            context=_read_arena_json(arena, ctx_off, ctx_len),
        ))
    return refs


# ── Public API ──────────────────────────────────────────────────────────────

def decode_extraction(meta: bytes, entities: bytes, edges: bytes,
                      refs: bytes, arena: bytes) -> FlatBuffers:
    """Decode a single file's flat-buffer extraction payload.

    Args:
        meta: 36-byte meta block.
        entities: Variable-length entity rows (ENTITY_ROW_SIZE each).
        edges: Variable-length edge rows (EDGE_ROW_SIZE each).
        refs: Variable-length ref rows (REF_ROW_SIZE each).
        arena: UTF-8 string arena.

    Returns:
        A FlatBuffers object with fully decoded entities, edges, and refs.

    Raises:
        ValueError: If ABI version mismatch or row-size mismatch.
    """
    # Decode meta
    abi_version = meta[0]
    if abi_version != KERNEL_ABI_VERSION:
        raise ValueError(
            f"ABI version mismatch: buffer has {abi_version}, "
            f"decoder expects {KERNEL_ABI_VERSION}"
        )

    entity_count = struct.unpack_from("<I", meta, 4)[0]
    edge_count = struct.unpack_from("<I", meta, 8)[0]
    ref_count = struct.unpack_from("<I", meta, 12)[0]
    # arena_len = struct.unpack_from("<I", meta, 16)[0]
    errors_off = struct.unpack_from("<I", meta, 20)[0]
    errors_len = struct.unpack_from("<I", meta, 24)[0]
    duration_ms = struct.unpack_from("<d", meta, 28)[0]

    # Validate row sizes
    if entities and len(entities) % ENTITY_ROW_SIZE != 0:
        raise ValueError(
            f"Entity buffer size {len(entities)} not a multiple of "
            f"ENTITY_ROW_SIZE ({ENTITY_ROW_SIZE})"
        )
    if edges and len(edges) % EDGE_ROW_SIZE != 0:
        raise ValueError(
            f"Edge buffer size {len(edges)} not a multiple of "
            f"EDGE_ROW_SIZE ({EDGE_ROW_SIZE})"
        )
    if refs and len(refs) % REF_ROW_SIZE != 0:
        raise ValueError(
            f"Ref buffer size {len(refs)} not a multiple of "
            f"REF_ROW_SIZE ({REF_ROW_SIZE})"
        )

    # Decode rows
    decoded_entities = [
        _decode_entity_row(entities, arena, i * ENTITY_ROW_SIZE)
        for i in range(entity_count)
    ]
    decoded_edges = _decode_edge_rows(edges, arena, edge_count)
    decoded_refs = _decode_ref_rows(refs, arena, ref_count)

    errors = _read_arena_str(arena, errors_off, errors_len)

    # Infer file path from first entity or default
    file_path = ""
    if decoded_entities:
        file_path = decoded_entities[0].file_path

    return FlatBuffers(
        abi_version=abi_version,
        file_path=file_path,
        entities=decoded_entities,
        edges=decoded_edges,
        refs=decoded_refs,
        errors_json=errors,
        duration_ms=duration_ms,
    )


def decode_extraction_from_bytes(data: bytes, arena: bytes) -> FlatBuffers:
    """Decode extraction when meta + entity + edge + ref data is concatenated.

    This expects: [meta: 36B] [entity rows] [edge rows] [ref rows]
    Counts come from the meta block.
    """
    if len(data) < META_SIZE:
        raise ValueError(f"Data too short for meta block: {len(data)} bytes")

    meta = data[:META_SIZE]
    entity_count = struct.unpack_from("<I", meta, 4)[0]
    edge_count = struct.unpack_from("<I", meta, 8)[0]
    ref_count = struct.unpack_from("<I", meta, 12)[0]

    pos = META_SIZE
    entity_bytes = data[pos:pos + entity_count * ENTITY_ROW_SIZE]; pos += entity_count * ENTITY_ROW_SIZE
    edge_bytes = data[pos:pos + edge_count * EDGE_ROW_SIZE]; pos += edge_count * EDGE_ROW_SIZE
    ref_bytes = data[pos:pos + ref_count * REF_ROW_SIZE]

    return decode_extraction(meta, entity_bytes, edge_bytes, ref_bytes, arena)


# ── Tests ───────────────────────────────────────────────────────────────────

import unittest


class TestFlatBufferDecoder(unittest.TestCase):
    """Verify the Python flat-buffer decoder matches the Rust encoder.

    These tests encode known data manually rather than calling into Rust,
    so they also serve as a secondary spec for the wire format.
    """

    def _make_meta(self, entity_count=0, edge_count=0, ref_count=0,
                   arena_len=0, duration_ms=0.0) -> bytes:
        return struct.pack(
            "<B3xIIIIIId",
            KERNEL_ABI_VERSION,
            entity_count, edge_count, ref_count, arena_len,
            NONE, 0,
            duration_ms,
        )

    def _make_entity_row(self, kind=0, name="", id_str="", file_path="",
                          line=0, exit_line=0, span=(0, 0), name_span=(0, 0),
                          docstring=None, signature=None) -> bytes:
        """Build a minimal 132-byte entity row."""
        arena = bytearray()
        def put(s: Optional[str]) -> Tuple[int, int]:
            if s is None:
                return (NONE, 0)
            off = len(arena)
            b = s.encode("utf-8")
            arena.extend(b)
            return (off, len(b))

        name_off, name_len = put(name)
        qname_off, qname_len = put(name)  # same as name for simple test
        id_off, id_len = put(id_str)
        doc_off, doc_len = put(docstring)
        sig_off, sig_len = put(signature)
        ret_off, ret_len = NONE, 0
        dec_off, dec_len = NONE, 0
        parent_off, parent_len = NONE, 0
        extra_off, extra_len = NONE, 0
        file_off, file_len = put(file_path)
        hash_off, hash_len = NONE, 0

        row = struct.pack(
            "<BB H I I I I" + "I I" * 11 + "I I I I I I",
            kind,            # entity kind index
            0,               # language
            0,               # flags
            line,            # start_line
            exit_line,       # end_line
            span[0],         # start_byte
            span[1],         # end_byte
            # 11 StrRef pairs
            name_off, name_len,
            qname_off, qname_len,
            id_off, id_len,
            doc_off, doc_len,
            sig_off, sig_len,
            ret_off, ret_len,
            dec_off, dec_len,
            parent_off, parent_len,
            extra_off, extra_len,
            file_off, file_len,
            hash_off, hash_len,
            # remaining fixed fields
            0,               # visibility
            0,               # metrics
            span[0], span[1],
            name_span[0], name_span[1],
        )
        assert len(row) == ENTITY_ROW_SIZE, f"Entity row size: {len(row)}"
        return row, bytes(arena)

    def test_abi_version_mismatch_raises(self):
        meta = struct.pack("<B3xIIIIIId", 99, 0, 0, 0, 0, NONE, 0, 0.0)
        with self.assertRaises(ValueError) as ctx:
            decode_extraction(meta, b"", b"", b"", b"")
        self.assertIn("ABI version", str(ctx.exception))

    def test_row_size_validation(self):
        meta = self._make_meta(entity_count=1)
        with self.assertRaises(ValueError) as ctx:
            decode_extraction(meta, b"short", b"", b"", b"")
        self.assertIn("ENTITY_ROW_SIZE", str(ctx.exception))

    def test_empty_extraction(self):
        meta = self._make_meta()
        result = decode_extraction(meta, b"", b"", b"", b"")
        self.assertEqual(result.abi_version, 1)
        self.assertEqual(len(result.entities), 0)
        self.assertEqual(len(result.edges), 0)
        self.assertEqual(len(result.refs), 0)

    def test_single_function_entity(self):
        row_bytes, arena = self._make_entity_row(
            kind=2,  # function
            name="foo",
            id_str="test.py::foo",
            file_path="test.py",
            line=10,
            exit_line=15,
            span=(100, 200),
            name_span=(104, 107),
            docstring="Does stuff.",
            signature="def foo(x: int) -> str:",
        )
        meta = self._make_meta(entity_count=1)
        result = decode_extraction(meta, row_bytes, b"", b"", arena)
        self.assertEqual(len(result.entities), 1)
        e = result.entities[0]
        self.assertEqual(e.kind, "function")
        self.assertEqual(e.name, "foo")
        self.assertEqual(e.id, "test.py::foo")
        self.assertEqual(e.file_path, "test.py")
        self.assertEqual(e.start_line, 10)
        self.assertEqual(e.end_line, 15)
        self.assertEqual(e.span, (100, 200))
        self.assertEqual(e.name_span, (104, 107))
        self.assertEqual(e.docstring, "Does stuff.")
        self.assertEqual(e.signature, "def foo(x: int) -> str:")

    def test_bool_flags_encoding(self):
        """Verify tri-state bool flags decode correctly."""
        # All absent
        flags = decode_bool_flags(0)
        self.assertIsNone(flags["is_async"])
        self.assertIsNone(flags["is_static"])

        # Present-true for FLAG_IS_ASYNC (bit 0=present, bit 1=true → pair 1 = 11₂)
        flags_raw = (1 << (FLAG_IS_ASYNC * 2)) | (1 << (FLAG_IS_ASYNC * 2 + 1))
        flags = decode_bool_flags(flags_raw)
        self.assertTrue(flags["is_async"])
        self.assertIsNone(flags["is_static"])

        # Present-false for FLAG_IS_STATIC (bit 0=present, bit 1=false → 01₂)
        flags_raw = (1 << (FLAG_IS_STATIC * 2))
        flags = decode_bool_flags(flags_raw)
        self.assertFalse(flags["is_static"])
        self.assertIsNone(flags["is_async"])

    def test_known_entity_kinds(self):
        """Verify entity kind wire indices match Rust."""
        expected = {
            0: "module", 1: "class", 2: "function",
            3: "method", 4: "import", 5: "constant", 6: "type_alias",
        }
        for idx, name in expected.items():
            self.assertEqual(ENTITY_KINDS[idx], name)

    def test_known_edge_kinds(self):
        """Verify edge kind wire indices match Rust."""
        expected = {
            0: "contains", 1: "calls", 2: "imports", 3: "extends",
            4: "implements", 5: "references", 6: "decorates",
            7: "instantiates", 8: "overrides", 9: "exports",
        }
        for idx, name in expected.items():
            self.assertEqual(EDGE_KINDS[idx], name)

    def test_row_size_constants(self):
        """Row sizes must match buffers.rs constants."""
        self.assertEqual(ENTITY_ROW_SIZE, 132)
        self.assertEqual(EDGE_ROW_SIZE, 52)
        self.assertEqual(REF_ROW_SIZE, 48)
        self.assertEqual(META_SIZE, 36)

    def test_decode_edge(self):
        """Decode a single edge with provenance."""
        arena = bytearray()
        def put(s: Optional[str]) -> Tuple[int, int]:
            if s is None:
                return (NONE, 0)
            off = len(arena)
            b = s.encode("utf-8")
            arena.extend(b)
            return (off, len(b))

        meta_off, meta_len = put('{"key":"value"}')
        src_off, src_len = put("test.py::foo")
        tgt_off, tgt_len = put("test.py::bar")
        props_off, props_len = NONE, 0

        edge = struct.pack(
            "<I I B B H I I I I I I I I I I",
            0, 1,                    # source_idx, target_idx
            1,                       # kind = calls
            3,                       # provenance = SignatureMatch (3)
            0,                       # padding
            42, NONE,                # line=42, column=NONE
            meta_off, meta_len,
            src_off, src_len,
            tgt_off, tgt_len,
            props_off, props_len,
        )
        assert len(edge) == EDGE_ROW_SIZE, f"Edge row size: {len(edge)}"

        meta = self._make_meta(edge_count=1)
        result = decode_extraction(meta, b"", edge, b"", bytes(arena))
        self.assertEqual(len(result.edges), 1)
        e = result.edges[0]
        self.assertEqual(e.source_idx, 0)
        self.assertEqual(e.target_idx, 1)
        self.assertEqual(e.kind, "calls")
        self.assertEqual(e.provenance, 3)
        self.assertEqual(e.line, 42)
        self.assertEqual(e.source_id, "test.py::foo")
        self.assertEqual(e.metadata, {"key": "value"})

    def test_decode_ref(self):
        """Decode a single unresolved reference."""
        arena = bytearray()
        def put(s: Optional[str]) -> Tuple[int, int]:
            if s is None:
                return (NONE, 0)
            off = len(arena)
            b = s.encode("utf-8")
            arena.extend(b)
            return (off, len(b))

        ref_name_off, ref_name_len = put("undefined_func")
        cand_off, cand_len = put("candidate1\0candidate2")
        from_id_off, from_id_len = put("test.py::caller")
        ctx_off, ctx_len = put('{"arity":2}')

        ref_row = struct.pack(
            "<I B B H I I" + "I I" * 4,
            0,                       # from_idx
            1,                       # kind = FunctionCall
            0,                       # flags
            0,                       # padding
            15, 3,                   # line=15, column=3
            ref_name_off, ref_name_len,
            cand_off, cand_len,
            from_id_off, from_id_len,
            ctx_off, ctx_len,
        )
        assert len(ref_row) == REF_ROW_SIZE, f"Ref row size: {len(ref_row)}"

        meta = self._make_meta(ref_count=1)
        result = decode_extraction(meta, b"", b"", ref_row, bytes(arena))
        self.assertEqual(len(result.refs), 1)
        r = result.refs[0]
        self.assertEqual(r.reference_name, "undefined_func")
        self.assertEqual(r.candidates, ["candidate1", "candidate2"])
        self.assertEqual(r.from_id, "test.py::caller")
        self.assertEqual(r.line, 15)
        self.assertEqual(r.column, 3)


if __name__ == "__main__":
    unittest.main()
