# Single-Pass Extraction — Implementation Plan

## Motivation

CodeRadar currently uses a two-pass extraction pipeline:

```
parse → QueryCursor → HashMap<node_id, TagInfo>     [tagger.rs]
     → walk 50K nodes → HashMap::get per node       [walker.rs: walk_node]
     → if tagged, emit_for_node() + push FrameKind  [walker.rs]
     → scan_for_fn_ref() per node inside functions  [walker.rs]
     → resolve_fn_ref_candidates()                   [walker.rs]
```

The recursive tree walk visits **every** AST node (50K+ for a 300KB TypeScript file) and performs a `HashMap::get()` on each — but only ~500–2,000 nodes are tagged by queries. That's **96%+ wasted lookups**. This is the dominant performance bottleneck on large codebases (codegraph-main: 18,000ms vs CodeGraph's 7,000ms on the same files with a single-pass hand-written walker).

The single-pass approach uses the tree-sitter query cursor (which already visits every tagged node) as the **sole** driver of extraction, eliminating the HashMap and the second walk.

## Target Architecture

```
parse → QueryCursor → for each capture (in document order):
         ├─ resolve context: walk parent chain → find container (class/function name)
         ├─ emit entity → record in emitted: HashMap<node_id, Emitted>
         ├─ maintain byte-range frame_stack for nesting context
         └─ skip subtree if container was emitted (no double-emit)
       → targeted fn_ref scan on function body subtrees only
       → resolve_fn_ref_candidates()
```

## Core Insight: `Node::parent()` Replaces the Walk Stack

The current recursive walk maintains a `Vec<WalkFrame>` stack — push on entering a class/function, pop on exit. With cursor-only iteration, we can't push/pop on entry/exit because we only visit tagged nodes, not all nodes.

Instead, we use `Node::parent()` (O(1) in tree-sitter) to reconstruct context on the fly:

1. When emitting a method: `parent()` → class node → look up its emitted entity → get class qname
2. When emitting a class: `parent()` → function/module node → look up → get enclosing qname
3. Average parent chain depth: 3–6 hops. Total hops per file: ~12K (2000 captures × 6) vs 50K+ tree walk nodes.

We also maintain a lightweight byte-range stack as an optimization:
- When we emit a class/function, push `(end_byte, qualified_name, FrameKind)` onto the stack
- Before processing each capture, pop entries where `node.start_byte() > end_byte` (node is past the container's end)
- This gives O(1) context resolution for the common case (sequential captures within the same container)

## Step-by-Step Implementation

### Step 1: Extract `emit_for_node` Into Standalone Functions

**Goal:** Refactor with zero behavior change. Move each tag branch out of the monolithic `emit_for_node` into pure functions that take explicit parameters instead of `&mut WalkContext`. This makes them callable from both the old walker (Step 1 verification) and the new cursor extractor (Step 2).

**Functions to extract:**

| Current logic | New function | Takes | Returns |
|---|---|---|---|
| `Tag::Class` branch (~200 lines) | `emit_class(node, source, file_path, parent_qname)` | `Node, &str, &str, &str` | `(ExtractedUnit, String /* qname */, Vec<UnresolvedRef> /* bases */)` |
| `Tag::Function` branch (~100 lines) | `emit_function(node, source, file_path, parent_qname, parent_class)` | `Node, &str, &str, &str, Option<&str>` | `(ExtractedUnit, String /* qname */)` |
| `Tag::Import` branch (~50 lines) | `emit_import(node, source, file_path)` | `Node, &str, &str` | `Vec<ExtractedUnit>` |
| `Tag::Call` branch (~30 lines) | `emit_call(node, source, units, current_fn_idx)` | `Node, &str, &[ExtractedUnit], Option<usize>` | `()` (modifies function's calls) |
| `Tag::Decorator` branch (~20 lines) | `emit_decorator(node, source)` | `Node, &str` | `String /* decorator name */` |
| `Tag::Field` branch (~40 lines) | `emit_field(node, source, file_path, parent_qname)` | `Node, &str, &str, &str` | `ExtractedUnit` |
| `Tag::Export` branch (~30 lines) | `emit_export(node, source)` | `Node, &str` | `String /* exported name */` |
| `Tag::Docstring` branch (~15 lines) | `emit_docstring(node, source)` | `Node, &str` | `(usize /* line */, String, usize /* end_line */)` |
| `Tag::Impl` branch (~30 lines) | `emit_impl(node, source, file_path)` | `Node, &str, &str` | `ExtractedUnit` |

**Verification:** After each function is extracted, run `cargo test --package core_indexer` (163 tests). All must pass before moving to the next function. If a test fails, the extraction was wrong — fix before continuing.

**Files touched:** `core_indexer/src/extract/walker.rs` only (add new functions, update `emit_for_node` to delegate to them).

### Step 2: Create `CursorExtractor` — New Single-Pass Engine

**File:** New `core_indexer/src/extract/single_pass.rs`

```rust
/// Entity emitted from a cursor capture, stored for context resolution.
struct Emitted {
    unit_idx: usize,        // index into units vec
    qualified_name: String,  // for building child qualified names
    kind: EmittedKind,       // Class | Function | Method | Module
    end_byte: usize,         // for byte-range stack
}

enum EmittedKind { Module, Class, Function }

/// Byte-range frame for nesting context.
struct Frame {
    end_byte: usize,
    qualified_name: String,
    kind: EmittedKind,
}

/// Single-pass extractor — cursor drives emission, parent chain resolves context.
pub struct CursorExtractor<'a> {
    source: &'a str,
    file_path: &'a str,
    compiled: &'a CompiledQuery,
    units: Vec<ExtractedUnit>,
    /// node_id → emitted entity info (for parent-chain context resolution)
    emitted: HashMap<usize, Emitted>,
    /// byte-range stack for O(1) context in sequential captures
    frames: Vec<Frame>,
    /// unresolved function-as-value candidates
    fn_ref_candidates: Vec<(usize, String, usize, usize)>,
    /// docstring info for preceding-docstring attachment
    docstrings: Vec<(usize, String, usize)>,
}
```

**`extract()` method:**

```rust
pub fn extract(&mut self, root_node: Node) -> Vec<ExtractedUnit> {
    // 1. Emit file-level module
    // 2. Iterate QueryCursor captures in document order
    // 3. For each capture:
    //    a. Pop frames past node.start_byte
    //    b. Resolve context: try frame_stack, fall back to parent-chain walk
    //    c. Call standalone emit function
    //    d. Record in self.emitted, push frame if container
    // 4. Targeted fn_ref scan on function body subtrees
    // 5. resolve_fn_ref_candidates()
    // 6. Return units
}
```

**Context resolution algorithm:**

```
fn resolve_context(node: Node, emitted: &HashMap, frames: &[Frame]) -> (String, Option<String>) {
    // parent_qname, parent_class
    // 1. Check frame_stack — O(1) for sequential captures
    for frame in frames.iter().rev() {
        if node.start_byte() < frame.end_byte {
            match frame.kind {
                EmittedKind::Class => {
                    return (frame.qualified_name.clone(), Some(frame.qualified_name.clone()));
                }
                EmittedKind::Function => {
                    // function inside function — walk further up for class
                    // continue checking earlier frames or parent chain
                }
                _ => break,
            }
        }
    }
    // 2. Walk parent chain — handles non-sequential or deeply nested captures
    let mut p = node.parent();
    let mut depth = 0;
    while let Some(ancestor) = p {
        if depth > 32 { break; } // safety limit
        if let Some(emitted) = emitted.get(&(ancestor.id() as usize)) {
            match emitted.kind {
                EmittedKind::Class => return (emitted.qualified_name.clone(), Some(emitted.qualified_name.clone())),
                EmittedKind::Function | EmittedKind::Module => {
                    // continue up for class container
                }
            }
        }
        p = ancestor.parent();
        depth += 1;
    }
    (String::new(), None) // file-scope
}
```

### Step 3: Targeted `scan_for_fn_ref`

Currently `walk_node` calls `scan_for_fn_ref` on **every** node when inside a function (50K+ nodes). The single-pass version iterates only over emitted function entities, walks their **body subtree** using the tree-sitter `Node::child()` traversal, and collects candidates. Same logic, dramatically fewer nodes visited:

```
for (node_id, emitted) in self.emitted.iter() {
    if emitted.kind == EmittedKind::Function {
        let func_node = /* retrieve node from stored node_id */;
        scan_subtree_for_fn_ref(func_node, emitted.unit_idx, &mut self.fn_ref_candidates);
    }
}
```

### Step 4: Switch Callers in `graph.rs`

Three call sites change:

```rust
// Before (index_file_inner):
let tagged = crate::extract::tagger::tag_tree(source, root_node, &compiled_query);
let units = crate::extract::walker::walk_and_extract(&tagged, root_node, file_path);

// After:
let units = crate::extract::single_pass::extract(source, root_node, &compiled_query, file_path);
```

Affected functions in `graph.rs`:
- `index_file_inner()` — line ~1460
- `extract_only()` — line ~1260
- `update_file()` — line ~1893

### Step 5: Verify — All 303 Tests Must Pass

```bash
cargo test --package core_indexer          # 163 Rust tests
python -m pytest tests/ -q                  # 140 Python tests
```

### Step 6: Benchmark

```bash
# Synthetic benchmark (parity expected: already 0.96×)
# codegraph-main (expect improvement from 2.77× → ~2.1–2.3×)
# CodeRadar self (expect same or slightly better: already 0.59×)
```

## Files Touched

| File | Change | Risk |
|------|--------|------|
| `core_indexer/src/extract/walker.rs` | Extract emit_* functions, keep old walker for reference | Low — refactor only |
| `core_indexer/src/extract/single_pass.rs` | **New file** — CursorExtractor | Medium — new code |
| `core_indexer/src/extract/mod.rs` | Add `pub mod single_pass` | Trivial |
| `core_indexer/src/graph.rs` | Switch 3 call sites to single_pass | Low |
| 18 `.scm` query files | May need enrichment if captures missing | Low — audit first |

## Risk Mitigation

| Risk | Probability | Mitigation |
|------|------------|------------|
| Parent-chain walk misses edge case (e.g., deeply nested) | Low | `debug_assert!(depth < 32)`; fallback to file-scope |
| Query captures insufficient for params/return types | Medium | Audit all 18 `.scm` files before switching; add missing captures |
| Byte-range stack order wrong for interleaved siblings | Low | Extensive testing with nested classes → functions → methods |
| fn_ref regression on callback detection | Medium | Targeted scan uses identical logic on function body subtrees |
| Performance regression (slower, not faster) | Low | Benchmark before/after; revert if no improvement |
| Memory: `HashMap<usize, Emitted>` replaces old `HashMap<usize, TagInfo>` | None | Same data structure, same size |

## Estimated Effort

- Step 1 (extract emit_* functions): 3–4 hours
- Step 2 (CursorExtractor): 4–6 hours
- Step 3 (targeted fn_ref): 1–2 hours
- Step 4 (switch callers): 30 minutes
- Step 5 (test + fix): 2–4 hours
- Step 6 (benchmark + tune): 1–2 hours

**Total: 2–3 days.**
