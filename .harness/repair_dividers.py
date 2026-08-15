# -*- coding: utf-8 -*-
"""One-shot repair of section-divider / doc-comment placement after the step-15 peels.

Every content line already exists exactly once (verified by multiset audit); this
script only RELOCATES lines so each divider sits at the top of the file that now
holds its section, and helper doc-comments sit above their helpers in mod.rs.
"""
import io, sys, re
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8')

T = 'core_indexer/src/graph/tests/'

def rd(p):
    return open(T + p, encoding='utf-8').read()

def wr(p, s):
    open(T + p, 'w', encoding='utf-8').write(s)

def lines_of(s):
    return s.split('\n')

def find_line(lines, needle, start=0):
    for i in range(start, len(lines)):
        if needle in lines[i]:
            return i
    raise AssertionError(f'needle not found: {needle!r}')

def remove_line(lines, idx):
    """Remove line idx plus one adjacent blank (prefer the preceding blank)."""
    del lines[idx]
    if idx > 0 and lines[idx - 1].strip() == '':
        del lines[idx - 1]
    elif idx < len(lines) and lines[idx].strip() == '':
        del lines[idx]

def insert_before(lines, anchor_idx, new_lines):
    lines[anchor_idx:anchor_idx] = new_lines

# ── locate the divider strings (exact, from current files) ────────────────
idx_rs  = rd('indexing_tests.rs').split('\n')
mro_rs  = rd('mro_tests.rs').split('\n')
call_rs = rd('call_graph_tests.rs').split('\n')
proj_rs = rd('projection_tests.rs').split('\n')
pers_rs = rd('persistence_tests.txt'.replace('.txt', '.rs')).split('\n')
emb_rs  = rd('embedding_tests.rs').split('\n')
inh_rs  = rd('inheritance_tests.rs').split('\n')
mod_rs  = rd('mod.rs').split('\n')

kotlin_div   = next(l for l in call_rs if 'Kotlin Indexing Tests' in l)
mro_div      = next(l for l in idx_rs if 'MRO / C3 Linearization' in l)
ruby_div     = next(l for l in mro_rs  if 'Ruby Indexing Tests' in l)
embed_div    = next(l for l in idx_rs  if 'Embedding Pipeline Tests' in l)
persist_div  = next(l for l in proj_rs if 'Persistence Tests' in l and '\u2500\u2500' in l)
importp_div  = next(l for l in idx_rs  if 'Import Parsing & Cross-File' in l)
macrame_div  = next(l for l in mod_rs  if 'v3.6: Macrame temporal query tests' in l)
phase1_div   = next(l for l in inh_rs  if 'Phase 1: traverse_bfs' in l)

idx_doc      = next(l for l in idx_rs if 'Helper: index a source string' in l)
snap_doc     = next(l for l in inh_rs if 'Build a fresh projection' in l)

# exact Macrame divider line from the pre-peel file (avoid re-typing dashes)
pre_lines = open('.harness/pre_peel_tests.txt', encoding='utf-8').read().split('\n')
clean_macrame = next(l for l in pre_lines if 'v3.6: Macrame temporal query tests' in l)

# ── 1. indexing_tests.rs: three moves out, two moves in ───────────────────
# remove MRO divider (sits above ruby tests) and Import-Parsing block
remove_line(idx_rs, find_line(idx_rs, 'MRO / C3 Linearization'))
i = find_line(idx_rs, 'Import Parsing & Cross-File')
remove_line(idx_rs, i)                                   # removes divider + one blank
remove_line(idx_rs, find_line(idx_rs, 'Helper: index a source string'))
# remove Embedding Pipeline divider (sits above import-parsing tests)
remove_line(idx_rs, find_line(idx_rs, 'Embedding Pipeline Tests'))

# insert Ruby divider before ruby tests
i = find_line(idx_rs, 'fn test_ruby_class_indexing(')
while i > 0 and not idx_rs[i - 1].strip().startswith('#['):
    i -= 1                                               # back up to its #[test]
insert_before(idx_rs, i, [ruby_div, ''])

# insert Import-Parsing divider before test_import_parsing_from_import
i = find_line(idx_rs, 'fn test_import_parsing_from_import(')
while i > 0 and not idx_rs[i - 1].strip().startswith('#['):
    i -= 1
insert_before(idx_rs, i, [importp_div, ''])

# insert Kotlin divider before first content (kotlin tests start the file)
i = find_line(idx_rs, 'fn test_kotlin_class_indexing(')
while i > 0 and not idx_rs[i - 1].strip().startswith('#['):
    i -= 1
insert_before(idx_rs, i, [kotlin_div, ''])

wr('indexing_tests.rs', '\n'.join(idx_rs))

# ── 2. mro_tests.rs: MRO divider in at top; Ruby divider out of tail ──────
remove_line(mro_rs, find_line(mro_rs, 'Ruby Indexing Tests'))
i = find_line(mro_rs, 'fn test_c3_mro_single_inheritance(')
while i > 0 and not mro_rs[i - 1].strip().startswith('#['):
    i -= 1
insert_before(mro_rs, i, [mro_div, ''])
wr('mro_tests.rs', '\n'.join(mro_rs))

# ── 3. call_graph_tests.rs: Kotlin divider out of tail ────────────────────
remove_line(call_rs, find_line(call_rs, 'Kotlin Indexing Tests'))
wr('call_graph_tests.rs', '\n'.join(call_rs))

# ── 4. projection_tests.rs: Persistence divider out of tail ───────────────
remove_line(proj_rs, find_line(proj_rs, 'Persistence Tests'))
wr('projection_tests.rs', '\n'.join(proj_rs))

# ── 5. persistence_tests.rs: Persistence divider at top; Macrame divider
#    before the first temporal test (clean UTF-8 copy of the mod.rs line) ──
i = find_line(pers_rs, 'fn test_persist_entities_no_store_returns_zero(')
while i > 0 and not pers_rs[i - 1].strip().startswith('#['):
    i -= 1
insert_before(pers_rs, i, [persist_div, ''])

# first temporal test = first fn after the Macrame divider in PRE layout;
# find it via the known test names (they are in this file now)
# all temporal tests are the last content of this file; anchor before the
# earliest one so the divider sits at the top of that section
temporal_fns = [i for i, l in enumerate(pers_rs) if re.match(r'^    fn test_temporal_\w+\(', l)]
assert temporal_fns, 'no temporal tests found'
first_t = min(temporal_fns)
j = first_t
while j > 0 and not pers_rs[j - 1].strip().startswith('#['):
    j -= 1
insert_before(pers_rs, j, [clean_macrame, ''])
wr('persistence_tests.rs', '\n'.join(pers_rs))

# ── 6. embedding_tests.rs: Embedding divider at top ───────────────────────
i = find_line(emb_rs, 'fn test_function_embedding_field(')
while i > 0 and not emb_rs[i - 1].strip().startswith('#['):
    i -= 1
insert_before(emb_rs, i, [embed_div, ''])
wr('embedding_tests.rs', '\n'.join(emb_rs))

# ── 7. inheritance_tests.rs: Phase-1 divider + snapshot doc out of tail ───
remove_line(inh_rs, find_line(inh_rs, 'Phase 1: traverse_bfs'))
remove_line(inh_rs, find_line(inh_rs, 'Build a fresh projection'))
wr('inheritance_tests.rs', '\n'.join(inh_rs))

# ── 8. mod.rs: drop mojibake Macrame divider; restore helper docs ─────────
mod_rs = [l for l in mod_rs if 'v3.6: Macrame temporal query tests' not in l]
i = find_line(mod_rs, 'fn index_source(')
insert_before(mod_rs, i, [idx_doc])
i = find_line(mod_rs, 'fn snapshot_from(')
insert_before(mod_rs, i, [phase1_div, '', snap_doc])
wr('mod.rs', '\n'.join(mod_rs))

print('repair complete')
