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

Offset layout pattern adapted from CodeGraph's decode.ts + layout.ts
(MIT/Apache-2.0, opticsWolf/codegraph). Named offsets instead of incremental
position counters make the decoder self-documenting and catch field-reorder
bugs at decode time rather than silently producing wrong data.
"""

from __future__ import annotations

import json
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

# ── Layout: Symbolic Byte Offsets ───────────────────────────────────────────
# Pattern adapted from CodeGraph's layout.ts — every field offset is named.
# If the Rust encoder changes, the offset mismatch surfaces immediately.

class MetaLayout:
    VERSION = 0       # u8
    # [3 bytes padding]
    ENTITY_COUNT = 4   # u32
    EDGE_COUNT = 8     # u32
    REF_COUNT = 12     # u32
    ARENA_LEN = 16     # u32
    ERRORS_OFF = 20    # u32 (NONE = no errors)
    ERRORS_LEN = 24    # u32
    DURATION_MS = 28   # f64


class EntityLayout:
    """132-byte entity row layout."""
    KIND = 0            # u8 — ENTITY_KINDS index
    LANGUAGE = 1        # u8
    FLAGS = 2           # u16 LE — BoolFlags bit pairs
    START_LINE = 4      # u32
    END_LINE = 8        # u32
    START_BYTE = 12     # u32
    END_BYTE = 16       # u32
    # StrRefs (offset: u32, length: u32 = 8 bytes each)
    NAME = 20
    QUALIFIED_NAME = 28
    ID = 36
    DOCSTRING = 44
    SIGNATURE = 52
    RETURN_TYPE = 60
    DECORATORS = 68
    PARENT_ID = 76
    EXTRA_JSON = 84
    FILE_PATH = 92
    CONTENT_HASH = 100
    # Fixed fields
    VISIBILITY = 108    # u32
    METRICS = 112       # u32 (reserved)
    SPAN_START = 116    # u32
    SPAN_END = 120      # u32
    NAME_SPAN_START = 124  # u32
    NAME_SPAN_END = 128    # u32


class EdgeLayout:
    """52-byte edge row layout."""
    SOURCE_IDX = 0      # u32 (NONE = use source_id_str)
    TARGET_IDX = 4      # u32 (NONE = use target_id_str)
    KIND = 8            # u8 — EDGE_KINDS index
    PROVENANCE = 9      # u8 — EdgeProvenance index
    # [2 bytes padding]
    LINE = 12           # u32 (NONE = absent)
    COLUMN = 16         # u32 (NONE = absent)
    METADATA_JSON = 20  # StrRef
    SOURCE_ID_STR = 28  # StrRef
    TARGET_ID_STR = 36  # StrRef
    PROPERTIES_JSON = 44  # StrRef


class RefLayout:
    """48-byte ref row layout."""
    FROM_IDX = 0        # u32 (NONE = use from_id_str)
    KIND = 4            # u8 — ReferenceKind index
    FLAGS = 5           # u8 — bit flags
    # [2 bytes padding]
    LINE = 8            # u32
    COLUMN = 12         # u32
    REFERENCE_NAME = 16  # StrRef
    CANDIDATES = 24     # StrRef (NUL-joined list)
    FROM_ID_STR = 32    # StrRef
    CONTEXT_JSON = 40   # StrRef


# Flag positions for BoolFlags bit-pair encoding
FLAG_IS_EXPORTED: int = 0
FLAG_IS_ASYNC: int = 1
FLAG_IS_STATIC: int = 2
FLAG_IS_ABSTRACT: int = 3
FLAG_IS_GENERATOR: int = 4
FLAG_IS_TYPE_CHECKING: int = 5

# Ref flags
REF_FLAG_FILE_PATH: int = 0x1  # ref carries its own filePath

# ── Decoded Types ────────────────────────────────────────────────────────────

@dataclass
class DecodedEntity:
    """A single entity decoded from the flat buffer.

    Only fields present in the buffer are set — absent fields are omitted
    from the dict representation, matching CodeGraph's optional-field pattern.
    """
    kind: str
    name: str
    id: str
    file_path: str
    language: int = 0
    start_line: int = 0
    end_line: int = 0
    start_byte: int = 0
    end_byte: int = 0
    span: Tuple[int, int] = (0, 0)
    name_span: Tuple[int, int] = (0, 0)

    # Optional — only set when present in buffer
    qualified_name: Optional[str] = None
    docstring: Optional[str] = None
    signature: Optional[str] = None
    return_type: Optional[str] = None
    decorators: List[str] = field(default_factory=list)
    parent_id: Optional[str] = None
    visibility: int = 0

    # BoolFlags (None = absent, True/False = present)
    is_async: Optional[bool] = None
    is_static: Optional[bool] = None
    is_exported: Optional[bool] = None
    is_abstract: Optional[bool] = None
    is_generator: Optional[bool] = None
    is_type_checking: Optional[bool] = None

    def to_dict(self) -> Dict[str, object]:
        """Convert to dict, omitting None/empty values.

        Pattern from CodeGraph's conditional field assignment:
        only emit keys for fields that are actually present.
        """
        d: Dict[str, object] = {
            "kind": self.kind,
            "name": self.name,
            "id": self.id,
            "file_path": self.file_path,
            "start_line": self.start_line,
            "end_line": self.end_line,
            "span": list(self.span),
            "name_span": list(self.name_span),
        }
        if self.qualified_name is not None:
            d["qualified_name"] = self.qualified_name
        if self.docstring is not None:
            d["docstring"] = self.docstring
        if self.signature is not None:
            d["signature"] = self.signature
        if self.return_type is not None:
            d["return_type"] = self.return_type
        if self.decorators:
            d["decorators"] = self.decorators
        if self.parent_id is not None:
            d["parent_id"] = self.parent_id
        if self.visibility:
            d["visibility"] = self.visibility
        return d


@dataclass
class DecodedEdge:
    """A single edge decoded from the flat buffer.

    Endpoints are resolved to entity IDs during decode via the id_by_row
    index. When the source/target is outside the current file, the fallback
    ID string is used (NONE sentinel on the row index).
    """
    source_id: str         # resolved entity ID
    target_id: str         # resolved entity ID
    kind: str
    provenance: int = 0
    line: Optional[int] = None
    column: Optional[int] = None
    metadata: Dict[str, object] = field(default_factory=dict)
    properties: Dict[str, object] = field(default_factory=dict)


@dataclass
class DecodedRef:
    """A single unresolved reference decoded from the flat buffer."""
    from_id: str           # resolved entity ID (or from_id_str fallback)
    reference_name: str
    kind: int = 0
    flags: int = 0
    line: int = 0
    column: int = 0
    candidates: List[str] = field(default_factory=list)
    context: Dict[str, object] = field(default_factory=dict)


@dataclass
class FlatBuffers:
    """Complete decoded flat-buffer payload for a single file extraction."""
    abi_version: int
    file_path: str
    entities: List[DecodedEntity]
    edges: List[DecodedEdge]
    refs: List[DecodedRef]
    errors: List[dict]              # parsed extraction errors
    duration_ms: float


# ── BoolFlags Decoder ───────────────────────────────────────────────────────

_FLAG_NAMES = {
    FLAG_IS_EXPORTED: "is_exported",
    FLAG_IS_ASYNC: "is_async",
    FLAG_IS_STATIC: "is_static",
    FLAG_IS_ABSTRACT: "is_abstract",
    FLAG_IS_GENERATOR: "is_generator",
    FLAG_IS_TYPE_CHECKING: "is_type_checking",
}


def decode_bool_flags(flags: int) -> Dict[str, Optional[bool]]:
    """Decode the tri-state bool flags bit-pair encoding.

    Each flag occupies 2 bits: bit 0 = present, bit 1 = value.
    Absent (00) → None, Present-False (01) → False, Present-True (11) → True.
    """
    result: Dict[str, Optional[bool]] = {}
    for pair_idx, name in _FLAG_NAMES.items():
        present = (flags >> (pair_idx * 2)) & 1
        value = (flags >> (pair_idx * 2 + 1)) & 1
        result[name] = (value != 0) if present else None
    return result


# ── Arena String Extraction ─────────────────────────────────────────────────

def _arena_str(arena: bytes, row: memoryview, at: int) -> Optional[str]:
    """Read a StrRef from the arena. Returns None when offset is NONE."""
    off = int.from_bytes(row[at:at + 4], "little")
    if off == NONE:
        return None
    length = int.from_bytes(row[at + 4:at + 8], "little")
    if length == 0:
        return None
    try:
        return arena[off:off + length].decode("utf-8")
    except (IndexError, UnicodeDecodeError):
        return None


def _arena_list(arena: bytes, row: memoryview, at: int) -> Optional[List[str]]:
    """Read a NUL-joined string list from the arena."""
    s = _arena_str(arena, row, at)
    if s is None:
        return None
    return [part for part in s.split("\0") if part]


def _arena_json(arena: bytes, row: memoryview, at: int) -> Optional[dict]:
    """Read a JSON-encoded field from the arena."""
    s = _arena_str(arena, row, at)
    if s is None:
        return None
    try:
        return json.loads(s)
    except json.JSONDecodeError:
        return None


def _u32opt(row: memoryview, at: int) -> Optional[int]:
    """Read an optional u32 — returns None when value is NONE.

    Pattern from CodeGraph's decode.ts: u32opt() for absent sentinel.
    """
    v = int.from_bytes(row[at:at + 4], "little")
    return None if v == NONE else v


# ── Public Decode API ───────────────────────────────────────────────────────

def decode_extraction(
    meta: bytes, entities: bytes, edges: bytes,
    refs: bytes, arena: bytes,
) -> FlatBuffers:
    """Decode a single file's flat-buffer extraction payload.

    Args:
        meta: 36-byte meta block.
        entities: Variable-length entity rows (ENTITY_ROW_SIZE each).
        edges: Variable-length edge rows (EDGE_ROW_SIZE each).
        refs: Variable-length ref rows (REF_ROW_SIZE each).
        arena: UTF-8 string arena.

    Returns:
        FlatBuffers with fully decoded and endpoint-resolved data.

    Raises:
        ValueError: On ABI version mismatch or row-size mismatch.
    """
    mv = memoryview(meta)

    abi_version = mv[MetaLayout.VERSION]
    if abi_version != KERNEL_ABI_VERSION:
        raise ValueError(
            f"ABI version mismatch: buffer has {abi_version}, "
            f"decoder expects {KERNEL_ABI_VERSION}"
        )

    entity_count = int.from_bytes(mv[MetaLayout.ENTITY_COUNT:MetaLayout.ENTITY_COUNT + 4], "little")
    edge_count = int.from_bytes(mv[MetaLayout.EDGE_COUNT:MetaLayout.EDGE_COUNT + 4], "little")
    ref_count = int.from_bytes(mv[MetaLayout.REF_COUNT:MetaLayout.REF_COUNT + 4], "little")
    errors_off = int.from_bytes(mv[MetaLayout.ERRORS_OFF:MetaLayout.ERRORS_OFF + 4], "little")
    errors_len = int.from_bytes(mv[MetaLayout.ERRORS_LEN:MetaLayout.ERRORS_LEN + 4], "little")
    duration_ms = struct.unpack_from("<d", meta, MetaLayout.DURATION_MS)[0]

    # Validate row sizes
    _validate_row_sizes(entities, edges, refs, entity_count, edge_count, ref_count)

    # Phase 1: decode entities — build id_by_row index for edge/ref resolution
    decoded_entities, id_by_row, file_path = _decode_entities(entities, arena, entity_count)

    # Phase 2: decode edges — resolve endpoints via id_by_row index
    decoded_edges = _decode_edges(edges, arena, edge_count, id_by_row)

    # Phase 3: decode refs — resolve from entity via id_by_row index
    decoded_refs = _decode_refs(refs, arena, ref_count, id_by_row)

    # Decode errors
    parsed_errors: List[dict] = []
    if errors_off != NONE and errors_len > 0:
        try:
            err_raw = arena[errors_off:errors_off + errors_len].decode("utf-8")
            parsed_errors = json.loads(err_raw)
        except (IndexError, UnicodeDecodeError, json.JSONDecodeError):
            pass

    return FlatBuffers(
        abi_version=abi_version,
        file_path=file_path,
        entities=decoded_entities,
        edges=decoded_edges,
        refs=decoded_refs,
        errors=parsed_errors,
        duration_ms=duration_ms,
    )


def _validate_row_sizes(
    entities: bytes, edges: bytes, refs: bytes,
    entity_count: int, edge_count: int, ref_count: int,
) -> None:
    """Validate row counts match buffer sizes."""
    expected_e = entity_count * ENTITY_ROW_SIZE
    if len(entities) != expected_e:
        raise ValueError(
            f"Entity buffer: expected {expected_e} bytes for {entity_count} rows, "
            f"got {len(entities)} (row size: {ENTITY_ROW_SIZE})"
        )
    expected_d = edge_count * EDGE_ROW_SIZE
    if len(edges) != expected_d:
        raise ValueError(
            f"Edge buffer: expected {expected_d} bytes for {edge_count} rows, "
            f"got {len(edges)} (row size: {EDGE_ROW_SIZE})"
        )
    expected_r = ref_count * REF_ROW_SIZE
    if len(refs) != expected_r:
        raise ValueError(
            f"Ref buffer: expected {expected_r} bytes for {ref_count} rows, "
            f"got {len(refs)} (row size: {REF_ROW_SIZE})"
        )


# ── Entity Decode ───────────────────────────────────────────────────────────

def _decode_entities(
    data: bytes, arena: bytes, count: int,
) -> Tuple[List[DecodedEntity], List[str], str]:
    """Decode all entity rows, returning (entities, id_by_row, file_path).

    id_by_row maps row index → entity ID for edge/ref endpoint resolution.
    Pattern from CodeGraph's decode.ts: build the index during entity decode.
    """
    entities: List[DecodedEntity] = []
    id_by_row: List[str] = []
    file_path = ""

    for i in range(count):
        row = memoryview(data)[i * ENTITY_ROW_SIZE:(i + 1) * ENTITY_ROW_SIZE]

        kind_idx = row[EntityLayout.KIND]
        kind = ENTITY_KINDS[kind_idx] if kind_idx < len(ENTITY_KINDS) else "unknown"
        language = row[EntityLayout.LANGUAGE]
        flags_raw = int.from_bytes(row[EntityLayout.FLAGS:EntityLayout.FLAGS + 2], "little")

        start_line = int.from_bytes(row[EntityLayout.START_LINE:EntityLayout.START_LINE + 4], "little")
        end_line = int.from_bytes(row[EntityLayout.END_LINE:EntityLayout.END_LINE + 4], "little")
        span_start = int.from_bytes(row[EntityLayout.SPAN_START:EntityLayout.SPAN_START + 4], "little")
        span_end = int.from_bytes(row[EntityLayout.SPAN_END:EntityLayout.SPAN_END + 4], "little")
        name_span_start = int.from_bytes(row[EntityLayout.NAME_SPAN_START:EntityLayout.NAME_SPAN_START + 4], "little")
        name_span_end = int.from_bytes(row[EntityLayout.NAME_SPAN_END:EntityLayout.NAME_SPAN_END + 4], "little")

        name = _arena_str(arena, row, EntityLayout.NAME) or ""
        entity_id = _arena_str(arena, row, EntityLayout.ID) or ""
        entity_file = _arena_str(arena, row, EntityLayout.FILE_PATH) or ""

        id_by_row.append(entity_id)
        if not file_path and entity_file:
            file_path = entity_file

        entity = DecodedEntity(
            kind=kind,
            name=name,
            id=entity_id,
            file_path=entity_file,
            language=language,
            start_line=start_line,
            end_line=end_line,
            start_byte=int.from_bytes(row[EntityLayout.START_BYTE:EntityLayout.START_BYTE + 4], "little"),
            end_byte=int.from_bytes(row[EntityLayout.END_BYTE:EntityLayout.END_BYTE + 4], "little"),
            span=(span_start, span_end),
            name_span=(name_span_start, name_span_end),
            visibility=int.from_bytes(row[EntityLayout.VISIBILITY:EntityLayout.VISIBILITY + 4], "little"),
        )

        # Conditional optional fields — only set when present
        qname = _arena_str(arena, row, EntityLayout.QUALIFIED_NAME)
        if qname is not None:
            entity.qualified_name = qname

        doc = _arena_str(arena, row, EntityLayout.DOCSTRING)
        if doc is not None:
            entity.docstring = doc

        sig = _arena_str(arena, row, EntityLayout.SIGNATURE)
        if sig is not None:
            entity.signature = sig

        ret = _arena_str(arena, row, EntityLayout.RETURN_TYPE)
        if ret is not None:
            entity.return_type = ret

        decs = _arena_list(arena, row, EntityLayout.DECORATORS)
        if decs is not None:
            entity.decorators = decs

        parent = _arena_str(arena, row, EntityLayout.PARENT_ID)
        if parent is not None:
            entity.parent_id = parent

        # BoolFlags
        flags = decode_bool_flags(flags_raw)
        for fname, fval in flags.items():
            if fval is not None:
                setattr(entity, fname, fval)

        entities.append(entity)

    return entities, id_by_row, file_path


# ── Edge Decode ─────────────────────────────────────────────────────────────

def _decode_edges(
    data: bytes, arena: bytes, count: int, id_by_row: List[str],
) -> List[DecodedEdge]:
    """Decode all edge rows, resolving endpoints via id_by_row index.

    When source_idx or target_idx is NONE, falls back to the string ID
    field. This handles edges that reference entities outside the current
    file (cross-file calls, imports, etc.).

    Pattern from CodeGraph's decode.ts:
      sourceIdx === NONE ? str(...EDGE.sourceIdStr) : idByRow[sourceIdx]
    """
    edges: List[DecodedEdge] = []
    for i in range(count):
        row = memoryview(data)[i * EDGE_ROW_SIZE:(i + 1) * EDGE_ROW_SIZE]

        source_idx = int.from_bytes(row[EdgeLayout.SOURCE_IDX:EdgeLayout.SOURCE_IDX + 4], "little")
        target_idx = int.from_bytes(row[EdgeLayout.TARGET_IDX:EdgeLayout.TARGET_IDX + 4], "little")

        # Resolve endpoints: use row index when not NONE, fallback to string ID
        source_id = (
            id_by_row[source_idx]
            if source_idx != NONE and source_idx < len(id_by_row)
            else (_arena_str(arena, row, EdgeLayout.SOURCE_ID_STR) or f"external:{source_idx}")
        )
        target_id = (
            id_by_row[target_idx]
            if target_idx != NONE and target_idx < len(id_by_row)
            else (_arena_str(arena, row, EdgeLayout.TARGET_ID_STR) or f"external:{target_idx}")
        )

        kind_idx = row[EdgeLayout.KIND]
        kind = EDGE_KINDS[kind_idx] if kind_idx < len(EDGE_KINDS) else "unknown"

        edge = DecodedEdge(
            source_id=source_id,
            target_id=target_id,
            kind=kind,
            provenance=row[EdgeLayout.PROVENANCE],
        )

        line = _u32opt(row, EdgeLayout.LINE)
        if line is not None:
            edge.line = line

        col = _u32opt(row, EdgeLayout.COLUMN)
        if col is not None:
            edge.column = col

        meta = _arena_json(arena, row, EdgeLayout.METADATA_JSON)
        if meta is not None:
            edge.metadata = meta

        props = _arena_json(arena, row, EdgeLayout.PROPERTIES_JSON)
        if props is not None:
            edge.properties = props

        edges.append(edge)

    return edges


# ── Ref Decode ──────────────────────────────────────────────────────────────

def _decode_refs(
    data: bytes, arena: bytes, count: int, id_by_row: List[str],
) -> List[DecodedRef]:
    """Decode all ref rows, resolving from entity via id_by_row index.

    When from_idx is NONE, falls back to from_id_str. The REF_FLAG_FILE_PATH
    flag indicates the ref carries its own filePath (for cross-file references
    extracted in bulk).
    """
    refs: List[DecodedRef] = []
    for i in range(count):
        row = memoryview(data)[i * REF_ROW_SIZE:(i + 1) * REF_ROW_SIZE]

        from_idx = int.from_bytes(row[RefLayout.FROM_IDX:RefLayout.FROM_IDX + 4], "little")

        from_id = (
            id_by_row[from_idx]
            if from_idx != NONE and from_idx < len(id_by_row)
            else (_arena_str(arena, row, RefLayout.FROM_ID_STR) or f"unknown:{from_idx}")
        )

        ref = DecodedRef(
            from_id=from_id,
            reference_name=_arena_str(arena, row, RefLayout.REFERENCE_NAME) or "",
            kind=row[RefLayout.KIND],
            flags=row[RefLayout.FLAGS],
            line=_u32opt(row, RefLayout.LINE) or 0,
            column=_u32opt(row, RefLayout.COLUMN) or 0,
        )

        cands = _arena_list(arena, row, RefLayout.CANDIDATES)
        if cands is not None:
            ref.candidates = cands

        ctx = _arena_json(arena, row, RefLayout.CONTEXT_JSON)
        if ctx is not None:
            ref.context = ctx

        refs.append(ref)

    return refs


# ── Legacy concatenated-data entry point ────────────────────────────────────

def decode_extraction_from_bytes(data: bytes, arena: bytes) -> FlatBuffers:
    """Decode when meta + entity + edge + ref data is concatenated as one buffer.

    Structure: [meta: 36B] [entity rows] [edge rows] [ref rows]
    """
    if len(data) < META_SIZE:
        raise ValueError(f"Data too short for meta block: {len(data)} bytes")

    mv = memoryview(data)
    entity_count = int.from_bytes(mv[MetaLayout.ENTITY_COUNT:MetaLayout.ENTITY_COUNT + 4], "little")
    edge_count = int.from_bytes(mv[MetaLayout.EDGE_COUNT:MetaLayout.EDGE_COUNT + 4], "little")
    ref_count = int.from_bytes(mv[MetaLayout.REF_COUNT:MetaLayout.REF_COUNT + 4], "little")

    pos = META_SIZE
    entity_bytes = bytes(data[pos:pos + entity_count * ENTITY_ROW_SIZE])
    pos += entity_count * ENTITY_ROW_SIZE
    edge_bytes = bytes(data[pos:pos + edge_count * EDGE_ROW_SIZE])
    pos += edge_count * EDGE_ROW_SIZE
    ref_bytes = bytes(data[pos:pos + ref_count * REF_ROW_SIZE])

    return decode_extraction(bytes(data[:META_SIZE]), entity_bytes, edge_bytes, ref_bytes, arena)


# ── Tests ───────────────────────────────────────────────────────────────────

import unittest


class TestFlatBufferDecoder(unittest.TestCase):
    """Verify the Python flat-buffer decoder against the Rust encoder contract.

    Tests encode known data manually (without calling into Rust), serving
    as a secondary spec for the wire format. Every constant, offset, and
    bit-packing rule is exercised.
    """

    def _make_meta(self, entity_count=0, edge_count=0, ref_count=0,
                   arena_len=0, errors=(), duration_ms=0.0,
                   arena: Optional[bytearray] = None) -> Tuple[bytes, bytes]:
        """Build a 36-byte meta block.

        If arena is provided, error JSON strings are written into it.
        Otherwise a fresh arena is created.
        """
        if arena is None:
            arena = bytearray()
        errors_json = json.dumps(list(errors)).encode("utf-8") if errors else b""
        err_off = NONE
        err_len = 0
        if errors_json:
            err_off = len(arena)
            err_len = len(errors_json)
            arena.extend(errors_json)
        return struct.pack(
            "<B3xIII I I I d",
            KERNEL_ABI_VERSION,
            entity_count, edge_count, ref_count, arena_len,
            err_off, err_len,
            duration_ms,
        ), bytes(arena)

    def _arena_put(self, arena: bytearray, s: Optional[str]) -> Tuple[int, int]:
        """Put a string in the arena, returning (offset, length)."""
        if s is None:
            return (NONE, 0)
        off = len(arena)
        b = s.encode("utf-8")
        arena.extend(b)
        return (off, len(b))

    def _make_entity_row(self, kind=0, name="", id_str="", file_path="",
                          line=0, exit_line=0, span=(0, 0), name_span=(0, 0),
                          docstring=None, signature=None,
                          arena: Optional[bytearray] = None) -> Tuple[bytes, bytes]:
        """Build a minimal 132-byte entity row.

        If arena is provided, strings are written into it (shared arena for
        multi-entity tests). Otherwise a fresh arena is created.
        """
        if arena is None:
            arena = bytearray()
        p = self._arena_put

        name_off, name_len = p(arena, name)
        qname_off, qname_len = p(arena, name)
        id_off, id_len = p(arena, id_str)
        doc_off, doc_len = p(arena, docstring)
        sig_off, sig_len = p(arena, signature)
        ret_off, ret_len = NONE, 0
        dec_off, dec_len = NONE, 0
        parent_off, parent_len = NONE, 0
        extra_off, extra_len = NONE, 0
        file_off, file_len = p(arena, file_path)
        hash_off, hash_len = NONE, 0

        row = struct.pack(
            "<BB H I I I I" + "I I" * 11 + "I I I I I I",
            kind,
            0,
            0,
            line,
            exit_line,
            span[0],
            span[1],
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
            0,  # visibility
            0,  # metrics
            span[0], span[1],
            name_span[0], name_span[1],
        )
        assert len(row) == ENTITY_ROW_SIZE, f"Entity row: {len(row)}"
        return row, bytes(arena)

    def _make_edge_row(self, source_idx=0, target_idx=1, kind=1,
                        provenance=0, line=42, source_id=None,
                        target_id=None,
                        arena: Optional[bytearray] = None) -> Tuple[bytes, bytes]:
        """Build a minimal 52-byte edge row."""
        if arena is None:
            arena = bytearray()
        p = self._arena_put

        meta_off, meta_len = NONE, 0
        src_off, src_len = p(arena, source_id)
        tgt_off, tgt_len = p(arena, target_id)
        props_off, props_len = NONE, 0

        line_val = line if line is not None else NONE
        col_val = NONE

        row = struct.pack(
            "<I I B B H I I" + "I I" * 4,
            source_idx, target_idx,
            kind, provenance,
            0,
            line_val, col_val,
            meta_off, meta_len,
            src_off, src_len,
            tgt_off, tgt_len,
            props_off, props_len,
        )
        assert len(row) == EDGE_ROW_SIZE, f"Edge row: {len(row)}"
        return row, bytes(arena)

    def _make_ref_row(self, from_idx=0, kind=1, flags=0, name="",
                       line=15, column=3, from_id=None,
                       candidates=None,
                       arena: Optional[bytearray] = None) -> Tuple[bytes, bytes]:
        """Build a minimal 48-byte ref row."""
        if arena is None:
            arena = bytearray()
        p = self._arena_put

        name_off, name_len = p(arena, name)
        cand_str = "\0".join(candidates) if candidates else None
        cand_off, cand_len = p(arena, cand_str)
        fid_off, fid_len = p(arena, from_id)
        ctx_off, ctx_len = NONE, 0

        row = struct.pack(
            "<I B B H I I" + "I I" * 4,
            from_idx, kind, flags,
            0,
            line, column,
            name_off, name_len,
            cand_off, cand_len,
            fid_off, fid_len,
            ctx_off, ctx_len,
        )
        assert len(row) == REF_ROW_SIZE, f"Ref row: {len(row)}"
        return row, bytes(arena)

    # ── Meta tests ──────────────────────────────────────────────────────

    def test_abi_version_mismatch_raises(self):
        meta, arena = self._make_meta()
        meta_bad = bytearray(meta)
        meta_bad[MetaLayout.VERSION] = 99
        with self.assertRaises(ValueError) as ctx:
            decode_extraction(bytes(meta_bad), b"", b"", b"", arena)
        self.assertIn("ABI version", str(ctx.exception))

    def test_empty_extraction(self):
        meta, arena = self._make_meta()
        result = decode_extraction(meta, b"", b"", b"", arena)
        self.assertEqual(result.abi_version, KERNEL_ABI_VERSION)
        self.assertEqual(len(result.entities), 0)
        self.assertEqual(len(result.edges), 0)
        self.assertEqual(len(result.refs), 0)

    def test_row_size_validation_exact(self):
        """Row buffers must be exact multiples of row size."""
        meta, arena = self._make_meta(entity_count=1)
        with self.assertRaises(ValueError) as ctx:
            decode_extraction(meta, b"short", b"", b"", arena)
        self.assertIn("Entity buffer", str(ctx.exception))

    # ── Entity decode tests ─────────────────────────────────────────────

    def test_single_function_entity(self):
        row_bytes, arena = self._make_entity_row(
            kind=2, name="foo", id_str="test.py::foo",
            file_path="test.py", line=10, exit_line=15,
            span=(100, 200), name_span=(104, 107),
            docstring="Does stuff.",
            signature="def foo(x: int) -> str:",
        )
        meta, _ = self._make_meta(entity_count=1)
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

    def test_id_by_row_index(self):
        """Verify id_by_row maps row index → entity ID."""
        arena = bytearray()
        row1, _ = self._make_entity_row(
            kind=2, name="foo", id_str="test.py::foo", file_path="test.py", arena=arena)
        row2, _ = self._make_entity_row(
            kind=1, name="Bar", id_str="test.py::Bar", file_path="test.py", arena=arena)

        combined_rows = row1 + row2
        meta, _ = self._make_meta(entity_count=2)
        result = decode_extraction(meta, combined_rows, b"", b"", bytes(arena))
        self.assertEqual(len(result.entities), 2)
        self.assertEqual(result.entities[0].id, "test.py::foo")
        self.assertEqual(result.entities[1].id, "test.py::Bar")

    def test_optional_fields_omitted_when_absent(self):
        """Entities without docstring/signature don't get None values."""
        row_bytes, arena = self._make_entity_row(
            kind=0, name="mod", id_str="mod", file_path="mod.py")
        meta, _ = self._make_meta(entity_count=1)
        result = decode_extraction(meta, row_bytes, b"", b"", arena)

        d = result.entities[0].to_dict()
        self.assertNotIn("docstring", d)
        self.assertNotIn("signature", d)

    def test_bool_flags_encoding(self):
        """Tri-state bool flags decode correctly."""
        flags = decode_bool_flags(0)
        self.assertIsNone(flags["is_async"])
        self.assertIsNone(flags["is_static"])

        # Present-true for FLAG_IS_ASYNC (bit 0=present, bit 1=true → 11₂)
        flags_raw = (1 << (FLAG_IS_ASYNC * 2)) | (1 << (FLAG_IS_ASYNC * 2 + 1))
        flags = decode_bool_flags(flags_raw)
        self.assertTrue(flags["is_async"])
        self.assertIsNone(flags["is_static"])

        # Present-false for FLAG_IS_STATIC (01₂)
        flags_raw = (1 << (FLAG_IS_STATIC * 2))
        flags = decode_bool_flags(flags_raw)
        self.assertFalse(flags["is_static"])
        self.assertIsNone(flags["is_async"])

    def test_known_entity_kinds(self):
        expected = {
            0: "module", 1: "class", 2: "function",
            3: "method", 4: "import", 5: "constant", 6: "type_alias",
        }
        for idx, name in expected.items():
            self.assertEqual(ENTITY_KINDS[idx], name)

    def test_known_edge_kinds(self):
        expected = {
            0: "contains", 1: "calls", 2: "imports", 3: "extends",
            4: "implements", 5: "references", 6: "decorates",
            7: "instantiates", 8: "overrides", 9: "exports",
        }
        for idx, name in expected.items():
            self.assertEqual(EDGE_KINDS[idx], name)

    def test_row_size_constants(self):
        self.assertEqual(ENTITY_ROW_SIZE, 132)
        self.assertEqual(EDGE_ROW_SIZE, 52)
        self.assertEqual(REF_ROW_SIZE, 48)
        self.assertEqual(META_SIZE, 36)

    # ── Edge decode tests ───────────────────────────────────────────────

    def test_decode_internal_edge(self):
        """Edge with row-index endpoints — resolved via id_by_row."""
        arena = bytearray()
        row1, _ = self._make_entity_row(
            kind=2, name="foo", id_str="test.py::foo", file_path="test.py", arena=arena)
        row2, _ = self._make_entity_row(
            kind=2, name="bar", id_str="test.py::bar", file_path="test.py", arena=arena)
        edge_row, _ = self._make_edge_row(
            source_idx=0, target_idx=1, kind=1, provenance=3, line=42, arena=arena)

        meta, _ = self._make_meta(entity_count=2, edge_count=1)
        result = decode_extraction(
            meta, row1 + row2, edge_row, b"", bytes(arena))
        self.assertEqual(len(result.edges), 1)
        e = result.edges[0]
        self.assertEqual(e.source_id, "test.py::foo")
        self.assertEqual(e.target_id, "test.py::bar")
        self.assertEqual(e.kind, "calls")
        self.assertEqual(e.provenance, 3)
        self.assertEqual(e.line, 42)

    def test_decode_external_edge(self):
        """Edge with NONE source_idx — falls back to source_id_str."""
        arena = bytearray()
        edge_row, _ = self._make_edge_row(
            source_idx=NONE, target_idx=NONE, kind=2, provenance=0, line=None,
            source_id="other.py::baz", target_id="other.py::qux", arena=arena)
        meta, _ = self._make_meta(edge_count=1, arena=arena)
        result = decode_extraction(meta, b"", edge_row, b"", bytes(arena))
        self.assertEqual(len(result.edges), 1)
        e = result.edges[0]
        self.assertEqual(e.source_id, "other.py::baz")
        self.assertEqual(e.target_id, "other.py::qux")
        self.assertEqual(e.kind, "imports")

    def test_edge_optional_fields(self):
        """Line/column are omitted when NONE."""
        arena = bytearray()
        edge_row, _ = self._make_edge_row(
            source_idx=NONE, target_idx=NONE, kind=1, provenance=0, line=None,
            source_id="a", target_id="b", arena=arena)
        meta, _ = self._make_meta(edge_count=1, arena=arena)
        result = decode_extraction(meta, b"", edge_row, b"", bytes(arena))
        self.assertIsNone(result.edges[0].line)
        self.assertIsNone(result.edges[0].column)

    # ── Ref decode tests ────────────────────────────────────────────────

    def test_decode_ref(self):
        arena = bytearray()
        ref_row, _ = self._make_ref_row(
            from_idx=NONE, kind=1, flags=0,
            name="undefined_func", line=15, column=3,
            from_id="test.py::caller",
            candidates=["candidate1", "candidate2"], arena=arena)
        meta, _ = self._make_meta(ref_count=1, arena=arena)
        result = decode_extraction(meta, b"", b"", ref_row, bytes(arena))
        self.assertEqual(len(result.refs), 1)
        r = result.refs[0]
        self.assertEqual(r.reference_name, "undefined_func")
        self.assertEqual(r.candidates, ["candidate1", "candidate2"])
        self.assertEqual(r.from_id, "test.py::caller")
        self.assertEqual(r.line, 15)
        self.assertEqual(r.column, 3)

    def test_internal_ref_resolution(self):
        """Ref from a known entity row index."""
        arena = bytearray()
        row1, _ = self._make_entity_row(
            kind=2, name="caller", id_str="test.py::caller", file_path="test.py", arena=arena)
        ref_row, _ = self._make_ref_row(
            from_idx=0, kind=1, name="target_func", line=10, arena=arena)

        meta, _ = self._make_meta(entity_count=1, ref_count=1)
        result = decode_extraction(meta, row1, b"", ref_row, bytes(arena))
        self.assertEqual(result.refs[0].from_id, "test.py::caller")

    # ── Error decode tests ──────────────────────────────────────────────

    def test_decode_errors(self):
        meta, arena = self._make_meta(errors=[
            {"message": "Parse error", "line": 42},
            {"message": "Unexpected token", "line": 99},
        ])
        result = decode_extraction(meta, b"", b"", b"", arena)
        self.assertEqual(len(result.errors), 2)
        self.assertEqual(result.errors[0]["message"], "Parse error")
        self.assertEqual(result.errors[0]["line"], 42)

    # ── Layout offset integrity ─────────────────────────────────────────

    def test_layout_offsets(self):
        """Verify all layout offsets are within row bounds."""
        # Entity: last field (name_span_end) spans bytes 128-131, end+4 = 132 = ENTITY_ROW_SIZE
        self.assertLessEqual(EntityLayout.NAME_SPAN_END + 4, ENTITY_ROW_SIZE)
        # Edge
        self.assertLessEqual(EdgeLayout.PROPERTIES_JSON + 8, EDGE_ROW_SIZE)
        # Ref
        self.assertLessEqual(RefLayout.CONTEXT_JSON + 8, REF_ROW_SIZE)
        # Meta
        self.assertLessEqual(MetaLayout.DURATION_MS + 8, META_SIZE)

    def test_u32opt_none_sentinel(self):
        """u32opt returns None when value is NONE (0xFFFFFFFF)."""
        buf = struct.pack("<I", NONE)
        self.assertIsNone(_u32opt(memoryview(buf), 0))

        buf = struct.pack("<I", 42)
        self.assertEqual(_u32opt(memoryview(buf), 0), 42)


if __name__ == "__main__":
    unittest.main()
