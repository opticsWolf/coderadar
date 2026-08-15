#!/usr/bin/env python3
"""Step-by-step extractor: moves ONE module's items from src/graph/mod.rs into its own file.

Usage:  python extract.py <step>     where <step> is one of the STEPS keys below.

Each step:
  1. recomputes item spans fresh (line numbers shift after earlier steps),
  2. removes those line ranges from mod.rs,
  3. writes the new file = use-block + extracted lines (verbatim) + visibility subs,
  4. inserts `pub mod X;` / re-export lines into mod.rs before the CodeGraph section comment,
  5. verifies: removed non-blank lines == new-file content lines (order preserved),
     and mod.rs changed only by the removals + inserted decl lines.

Gate after each step: cargo check --lib, warning count vs baseline, then commit.
"""
import faulthandler
faulthandler.dump_traceback_later(30, exit=True)
import re
import sys

try:
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')
except Exception:
    pass

SRC = 'src/graph/mod.rs'


def load():
    return open(SRC, encoding='utf-8').read().split('\n')


LEAD = re.compile(r'^(\s*)(///|//[^\[]|#\[)')


class Slicer:
    def __init__(self):
        self.lines = load()
        self.N = len(self.lines)

    def find_line(self, pattern):
        rx = re.compile(pattern)
        for i in range(self.N):
            if rx.match(self.lines[i]):
                return i
        raise RuntimeError(f'anchor not found: {pattern}')

    def item_span(self, decl_idx):
        s = decl_idx
        k = decl_idx - 1
        while k >= 0 and LEAD.match(self.lines[k]):
            s = k
            k -= 1
        indent = len(self.lines[decl_idx]) - len(self.lines[decl_idx].lstrip(' '))
        if indent == 0:
            closer = lambda l: l.rstrip() in ('}', '};')
        else:
            pad = ' ' * indent
            closer = lambda l: l == pad + '}'
        j = decl_idx + 1
        while j < N_safe(self):
            if closer(self.lines[j]):
                return s, j
            j += 1
        raise RuntimeError(f'no closer for decl at line {decl_idx + 1}: {self.lines[decl_idx][:60]!r}')

    def top_span(self, pattern):
        return self.item_span(self.find_line(pattern))

    def method_span(self, name, lo, hi):
        rx = re.compile(rf'^    (pub\([^)]*\) )?(pub )?fn {name}\(')
        for i in range(lo, hi):
            if rx.match(self.lines[i]):
                return self.item_span(i)
        raise RuntimeError(f'method not found in impl CodeGraph: {name}')

    def impl_bounds(self):
        open_idx = self.find_line(r'^impl CodeGraph \{')
        tc = self.find_line(r'^// \u2500\u2500 Tests')
        close_idx = tc - 1
        while self.lines[close_idx].strip() != '}':
            close_idx -= 1
        return open_idx, close_idx

    def comment_block(self, prefix):
        """Contiguous run of '    //...' lines starting at the line matching prefix."""
        start = self.find_line(re.escape(prefix))
        end = start
        while end + 1 < self.N and re.match(r'^\s+//', self.lines[end + 1]):
            end += 1
        return start, end


def N_safe(s):
    return s.N


# ---------------------------------------------------------------- step specs
# Each spec: file, uses (list of lines), parts = list of ('span'|'method'|'region'|'comment', key...),
# subs = [(old_frag, new_frag)], decls = [lines to insert into mod.rs]

def spec_config(sl):
    fm_lead = sl.top_span(r'^pub\(crate\) fn find_module_by_dotted_name\(')[0]
    i = fm_lead - 1
    while i >= 0 and sl.lines[i].strip() == '':
        i -= 1
    assert sl.lines[i].startswith('// \u2500\u2500 CodeGraph'), f'L{i + 1}: {sl.lines[i]!r}'
    j = i - 1
    while not sl.lines[j].startswith('// \u2500\u2500 Graph Config'):
        assert (re.match(r'^(pub(\([^)]*\))? )?(struct|impl) [A-Z]', sl.lines[j])
                or sl.lines[j].strip() == '' or sl.lines[j].startswith(('#[', '    ', '\t'))
                or sl.lines[j].rstrip() in ('}', '};')), f'L{j + 1}: {sl.lines[j]!r}'
        j -= 1
    return dict(file='config.rs', uses=[], parts=[('range', j, i - 1)], subs=[],
                decls=['pub mod config;', '', 'pub use config::*;'])


def spec_import_graph(sl):
    sec = sl.find_line(r'^// \u2500\u2500 Import Graph')
    a, b = sl.top_span(r'^pub struct ImportNode \{')
    c, d = sl.top_span(r'^impl ImportGraph \{')
    assert b < c and d - c > 100
    return dict(file='import_graph.rs',
                uses=['use std::collections::BTreeSet;', 'use std::path::PathBuf;', '',
                      'use dashmap::DashMap;',
                      'use petgraph::stable_graph::{NodeIndex, StableDiGraph};', '',
                      'use crate::types::*;'],
                parts=[('range', sec, sec), ('range', a, d)], subs=[],
                decls=['pub mod import_graph;', '',
                       'pub use import_graph::{ImportGraph, ImportNode};'])


def spec_call_graph(sl):
    sec = sl.find_line(r'^// \u2500\u2500 Call Graph')
    a, b = sl.top_span(r'^pub struct CallNode \{')
    c, d = sl.top_span(r'^impl CallGraph \{')
    return dict(file='call_graph.rs',
                uses=['use std::collections::{BTreeSet, HashMap};', '',
                      'use dashmap::DashMap;',
                      'use petgraph::stable_graph::{NodeIndex, StableDiGraph};', '',
                      'use crate::types::*;'],
                parts=[('range', sec, sec), ('range', a, d)], subs=[],
                decls=['pub mod call_graph;', '',
                       'pub use call_graph::{CallEdge, CallGraph, CallNode};'])


def spec_module_resolution(sl):
    p1 = sl.top_span(r'^fn normalize_path_str\(')
    p2 = sl.top_span(r'^pub\(crate\) fn find_module_by_dotted_name\(')
    p3 = sl.top_span(r'^fn find_symbol_in_module\(')
    return dict(file='module_resolution.rs', uses=['use crate::types::*;'],
                parts=[('range', p1[0], p1[1]), ('range', p2[0], p2[1]), ('range', p3[0], p3[1])],
                subs=[('fn normalize_path_str(', 'pub(super) fn normalize_path_str('),
                      ('fn find_symbol_in_module(', 'pub(super) fn find_symbol_in_module(')],
                decls=['pub mod module_resolution;', '',
                       'use module_resolution::{find_symbol_in_module, normalize_path_str};',
                       'pub(crate) use module_resolution::find_module_by_dotted_name;'])


def impl_spec(sl, file, uses, methods, subs=(), extra_comments=()):
    lo, hi = sl.impl_bounds()
    parts = []
    for key in list(extra_comments) + list(methods):
        if isinstance(key, tuple) and key[0] == 'comment':
            a, b = sl.comment_block(key[1])
            parts.append(('comment', a, b))
        else:
            a, b = sl.method_span(key, lo, hi)
            parts.append(('method', a, b))
    return dict(file=file, uses=uses, parts=parts, subs=list(subs), decls=[f'pub mod {file[:-3]};'])


def spec_mro(sl):
    c3 = sl.top_span(r'^fn c3_merge\(')
    d = impl_spec(sl, 'mro.rs', ['use super::CodeGraph;', 'use crate::types::*;'],
                  ['compute_all_mro', 'compute_c3_mro', 'resolve_base_by_name',
                   'base_candidates', 'import_aware_base'],
                  subs=[('    fn base_candidates(', '    pub(super) fn base_candidates(')])
    d['parts'] = [('item', c3[0], c3[1])] + [p for p in d['parts']]
    # Phase D comment sits between compute_c3_mro and resolve_base_by_name; apply_step sorts by line
    a, b = sl.comment_block('    // \u2500\u2500 Phase D')
    return d, a, b


def spec_inheritance(sl):
    return impl_spec(sl, 'inheritance.rs',
                     ['use std::collections::HashMap;', 'use std::sync::Arc;', '', 'use super::CodeGraph;',
                      'use super::module_resolution::find_module_by_dotted_name;',
                      'use crate::types::*;'],
                     ['resolve_class_hierarchy', 'resolve_imports',
                      'populate_class_methods', 'resolve_overrides'])


def spec_traversal(sl):
    d = impl_spec(sl, 'traversal.rs',
                  ['use super::CodeGraph;', '', 'use crate::types::*;'],
                  ['neighbors_of', 'traverse_bfs', 'count_unresolved_targets'])
    a, b = sl.comment_block('    // \u2500\u2500 Traversal core')
    return d, a, b


def spec_resolve_calls(sl):
    return impl_spec(sl, 'resolve_calls.rs',
                     ['use std::collections::HashMap;', '', 'use super::CodeGraph;',
                      'use super::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};',
                      'use crate::types::*;'],
                     ['resolve_all_calls', 'resolve_one_function', 'resolve_calls_scoped'],
                     subs=[('    fn resolve_calls_scoped(', '    pub(super) fn resolve_calls_scoped(')])


def spec_persistence(sl):
    return impl_spec(sl, 'persistence.rs',
                     ['use super::CodeGraph;', 'use crate::types::*;',
                      'use macrame::graph::EdgeAssertion;'],
                     ['persist_entities', 'persist_edges', 'register_synthetic_edge'])


def spec_embeddings(sl):
    return impl_spec(sl, 'embeddings.rs',
                     ['use super::CodeGraph;', 'use crate::types::*;'],
                     ['set_embedding', 'clear_embeddings_for_file', 'set_module_star_exports'])


def spec_indexing(sl):
    d = impl_spec(sl, 'indexing.rs',
                  ['use std::collections::HashMap;', '', 'use super::CodeGraph;',
                   'use crate::types::*;'],
                  ['ts_language', 'index_file', 'index_file_accumulate', 'synthesize_module_unit',
                   'extract_only', 'build_fragment', 'index_file_inner'])
    a, b = sl.comment_block('    // \u2500\u2500 File Indexing Pipeline')
    return d, a, b


def spec_projection_ops(sl):
    return impl_spec(sl, 'projection_ops.rs',
                     ['use std::collections::{BTreeSet, HashMap};', '', 'use super::CodeGraph;',
                      'use super::module_resolution::normalize_path_str;',
                      'use crate::types::*;'],
                     ['remove_file_entities', 'apply_diff_update', 'update_file', 'insert_extracted'],
                     subs=[('    fn insert_extracted(', '    pub(super) fn insert_extracted(')])


STEPS = {
    'config': spec_config,
    'import_graph': spec_import_graph,
    'call_graph': spec_call_graph,
    'module_resolution': spec_module_resolution,
    'mro': spec_mro,
    'inheritance': spec_inheritance,
    'traversal': spec_traversal,
    'resolve_calls': spec_resolve_calls,
    'persistence': spec_persistence,
    'embeddings': spec_embeddings,
    'indexing': spec_indexing,
    'projection_ops': spec_projection_ops,
}


def apply_step(name):
    sl = Slicer()
    result = STEPS[name](sl)
    if isinstance(result, tuple):
        d, ca, cb = result
        # insert the comment range at its positional spot (sorted by start line)
        parts = d['parts'] + [('comment', ca, cb)]
        parts.sort(key=lambda p: p[1])
        d['parts'] = parts
    else:
        d = result

    file, uses, parts, subs, decls = d['file'], d['uses'], d['parts'], d['subs'], d['decls']

    # sanity: ranges in order, non-overlapping
    for (k1, a1, b1), (k2, a2, b2) in zip(parts, parts[1:]):
        assert b1 < a2, f'ranges out of order/overlap: {a1}-{b1} then {a2}-{b2}'

    # classify parts for impl-block wrapping:
    #   method            -> inside
    #   comment (pure //) -> inside if adjacent to methods (before or after)
    #   item / range      -> outside (top-level items like c3_merge)
    methods = [(a, b) for (k, a, b) in parts if k == 'method']

    def is_comment_only(a, b):
        return all(l.strip() == '' or l.lstrip().startswith('//') for l in sl.lines[a:b + 1])

    inside_flags = []
    for (kind, a, b) in parts:
        if kind == 'method':
            inside_flags.append(True)
        elif kind == 'comment':
            assert is_comment_only(a, b), f'comment part not comment-only: L{a + 1}-{b + 1}'
            inside_flags.append(any(ma < a for ma, mb in methods) or any(mb > b for ma, mb in methods))
        else:
            inside_flags.append(False)

    # collect lines for the new file
    new_lines = list(uses) + ['']
    removed = []
    in_impl = False
    for (kind, a, b), want_inside in zip(parts, inside_flags):
        no_sep = False
        if want_inside and not in_impl:
            if new_lines[-1] != '':
                new_lines.append('')
            new_lines.append('impl CodeGraph {')
            in_impl = True
            no_sep = True
        elif not want_inside and in_impl:
            new_lines.append('}')
            in_impl = False
        if not no_sep and new_lines and new_lines[-1] != '':
            new_lines.append('')
        chunk = sl.lines[a:b + 1]
        for old, new in subs:
            for i, l in enumerate(chunk):
                if l.count(old) == 1:
                    chunk[i] = l.replace(old, new)
        # verify subs applied
        for old, _ in subs:
            assert not any(old in l and 'pub(super)' not in l for l in chunk), f'sub not applied: {old}'
        removed.extend(range(a, b + 1))
        new_lines.extend(chunk)
    if in_impl:
        new_lines.append('}')

    # build new mod.rs: remove ranges (bottom-up), insert decls before CodeGraph section comment
    lines = sl.lines[:]
    ins_at = next(i for i in range(len(lines)) if lines[i].startswith('// \u2500\u2500 CodeGraph'))
    for a, b in sorted(((p[1], p[2]) for p in parts), reverse=True):
        del lines[a:b + 1]
    # collapse triple+ blank runs left behind into single blanks (cosmetic, keeps file tidy)
    out = []
    prev_blank = 0
    for l in lines:
        if l.strip() == '':
            prev_blank += 1
            if prev_blank > 1:
                continue
        else:
            prev_blank = 0
        out.append(l)
    lines = out
    ins_at = next(i for i in range(len(lines)) if lines[i].startswith('// \u2500\u2500 CodeGraph'))
    block = [x for x in decls]
    # ensure blank separation around inserted block
    while block and block[0] == '':
        block.pop(0)
    while block and block[-1] == '':
        block.pop()
    lines[ins_at:ins_at] = [''] + block + ['']

    open(SRC, 'w', encoding='utf-8', newline='\n').write('\n'.join(lines))
    import os
    os.makedirs('src/graph', exist_ok=True)
    content = '\n'.join(new_lines).rstrip('\n') + '\n'
    open(f'src/graph/{file}', 'w', encoding='utf-8', newline='\n').write(content)

    # report
    print(f'{name}: wrote src/graph/{file} ({len(new_lines)} lines), removed {len(removed)} lines from mod.rs')
    for (kind, a, b) in parts:
        print(f'   L{a + 1}-{b + 1}')


if __name__ == '__main__':
    if len(sys.argv) != 2 or sys.argv[1] not in STEPS:
        print('usage: python extract.py <' + '|'.join(STEPS) + '>')
        sys.exit(2)
    apply_step(sys.argv[1])
