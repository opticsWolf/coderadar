#!/usr/bin/env python3
"""Step 15 peeler: move named tests from src/graph/tests/mod.rs into a new
sibling test file, verbatim (indentation preserved — string literals may span
lines). One target file per invocation; recompute spans fresh each time.

Usage: python3 peel.py <target_stem>            e.g. peel.py query_compile_tests
Specs live in SPECS below: title + extra imports + test fn names.
"""
import io, os, re, sys

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')

ROOT = 'src/graph/tests'
MOD = f'{ROOT}/mod.rs'

SPECS = {
    'query_compile_tests': dict(
        title='Query compilation smoke test',
        extra=[],
        tests=['test_all_queries_compile_without_errors']),
    'import_graph_tests': dict(
        title='ImportGraph resolution tests',
        extra=['use std::path::PathBuf;'],
        tests=['test_import_graph_add_and_find', 'test_import_graph_remove_file',
               'test_import_graph_transitive', 'test_multi_hop_import_resolution',
               'test_star_exports_wildcard_import', 'test_extension_agnostic_module_resolution',
               'test_import_graph_nonexistent', 'test_alias_aware_module_resolution']),
    'call_graph_tests': dict(
        title='CallGraph traversal tests',
        extra=[],
        tests=['test_call_graph_find_callers', 'test_call_graph_chain',
               'test_call_graph_cycle_safe', 'test_codegraph_snapshot',
               'test_codegraph_callers_of_empty']),
    'mro_tests': dict(
        title='C3 MRO tests',
        extra=[],
        tests=['test_c3_mro_single_inheritance', 'test_c3_mro_multiple_inheritance',
               'test_mro_method_resolution', 'test_c3_diamond']),
    'traversal_tests': dict(
        title='Traversal / BFS tests',
        extra=[],
        tests=['test_count_unresolved_targets', 'test_traverse_calls_downstream_depth',
               'test_traverse_calls_upstream', 'test_traverse_cycle_terminates',
               'test_traverse_diamond_one_entry_per_node',
               'test_traverse_max_depth_zero_returns_only_start',
               'test_traverse_empty_edge_kinds_returns_only_start',
               'test_traverse_imports_upstream_nonempty',
               'test_traverse_extends_downstream_via_resolved_bases',
               'test_traverse_overrides_upstream_from_base',
               'test_traverse_inherits_alias_for_extends']),
    'inheritance_tests': dict(
        title='Inheritance / base-resolution tests',
        extra=[],
        tests=['test_resolve_class_hierarchy_populates_subclasses',
               'test_resolve_imports_populates_importers',
               'test_language_family_filters_base_candidates',
               'test_ambiguous_base_emits_finding',
               'test_ts_typeonly_import_aware_base_resolution',
               'test_import_aware_base_resolution',
               'test_resolve_overrides_populates_overridden_by',
               'test_builtin_type_bases_filtered']),
    'embedding_tests': dict(
        title='Embedding store / similarity tests',
        extra=[],
        tests=['test_function_embedding_field', 'test_cosine_similarity',
               'test_set_embedding_stores_vector', 'test_set_embedding_entity_not_found',
               'test_set_embedding_overwrites_existing',
               'test_search_similar_after_set_embedding', 'test_set_embedding_empty_vector']),
    'persistence_tests': dict(
        title='Macrame persistence / temporal tests',
        extra=[],
        tests=['test_persist_entities_no_store_returns_zero',
               'test_persist_edges_no_store_returns_zero',
               'test_persist_entities_with_index', 'test_persist_edges_with_resolved_calls',
               'test_synthetic_edge_registration', 'test_synthetic_edge_roundtrip_query',
               'test_temporal_concepts_persisted', 'test_temporal_reconstruct_after_index',
               'test_temporal_edge_persistence_across_indexes',
               'test_persist_edges_emits_imports_and_extends',
               'test_temporal_synthetic_edge_persistence']),
    'projection_tests': dict(
        title='Projection diff/update tests',
        extra=[],
        tests=['test_update_file_adds_entities', 'test_update_file_removes_entities']),
    'indexing_tests': dict(
        title='Per-language indexing pipeline tests',
        extra=[],
        tests=['test_kotlin_class_indexing', 'test_kotlin_call_indexing',
               'test_typescript_function_indexing', 'test_typescript_class_indexing',
               'test_member_expression_base_is_stringified_not_dropped',
               'test_go_function_indexing', 'test_go_struct_indexing',
               'test_java_class_indexing', 'test_java_call_indexing',
               'test_cpp_function_indexing', 'test_cpp_class_indexing',
               'test_ruby_class_indexing', 'test_ruby_module_indexing',
               'test_php_class_indexing', 'test_php_call_indexing',
               'test_csharp_class_indexing', 'test_csharp_call_indexing',
               'test_go_method_receiver', 'test_swift_function_indexing',
               'test_swift_class_indexing', 'test_scala_class_indexing',
               'test_lua_function_indexing', 'test_lua_table_indexing',
               'test_elixir_module_indexing', 'test_zig_function_indexing',
               'test_zig_struct_indexing', 'test_r_function_indexing',
               'test_fn_ref_assignment_callback', 'test_fn_ref_return_value',
               'test_fn_ref_no_false_positives', 'test_fn_ref_argument_list',
               'test_fn_ref_dict_values', 'test_literal_receiver_skipped',
               'test_grammar_kind_python_class', 'test_grammar_kind_rust_struct',
               'test_grammar_kind_typescript_class', 'test_grammar_kind_swift_struct',
               'test_grammar_kind_swift_class', 'test_grammar_kind_zig_struct',
               'test_fn_ref_cross_file_import', 'test_rust_struct_indexing',
               'test_rust_method_resolution', 'test_class_methods_populated_27',
               'test_import_parsing_from_import', 'test_import_parsing_module_import',
               'test_cross_file_resolution_same_dir', 'test_cross_file_resolution_nested_package',
               'test_module_children_resolution', 'test_parameter_annotations_extracted',
               'test_return_type_builtin_filtered']),
}


def find_fn(lines, name):
    """Return (start, end) 0-based inclusive span for `fn name(` at 4-space indent.
    start extends upward over contiguous attr/comment lines; end stops before the
    next same-indent fn/attr anchor and strips trailing blanks."""
    pat = re.compile(rf'^\s*fn {re.escape(name)}\(')
    idx = None
    for i, l in enumerate(lines):
        if pat.match(l) and not l.startswith('        '):
            assert idx is None, f'duplicate fn {name}'
            idx = i
    assert idx is not None, f'fn not found: {name}'
    s = idx
    while s > 0:
        prev = lines[s - 1].strip()
        if prev.startswith('#[') or prev.startswith('//'):
            s -= 1
        else:
            break
    j = idx + 1
    while j < len(lines):
        lj = lines[j]
        if (lj.startswith('    fn ') or lj.startswith('    #[')) and not lj.startswith('        '):
            break
        j += 1
    # back up over the NEXT item's leading attrs/comments so they stay with it
    k = j
    while k > idx + 1:
        prev = lines[k - 1].strip()
        if prev.startswith('#[') or prev.startswith('//'):
            k -= 1
        else:
            break
    e = k - 1
    while e > s and lines[e].strip() == '':
        e -= 1
    return s, e


def main():
    stem = sys.argv[1]
    spec = SPECS[stem]
    lines = open(MOD, encoding='utf-8').read().split('\n')

    spans = []
    for name in spec['tests']:
        s, e = find_fn(lines, name)
        spans.append((s, e, name))
    # resolve overlaps: a comment block between two tests is claimed by BOTH
    # the forward-scan of the earlier span and the backward extension of the
    # later one. Positional rule (same as production extraction): comments go
    # with the FOLLOWING item, so trim the earlier span's end.
    spans.sort()
    for i in range(len(spans) - 1):
        if spans[i][1] >= spans[i + 1][0]:
            b = spans[i + 1][0] - 1
            while b > spans[i][0] and lines[b].strip() == '':
                b -= 1
            spans[i] = (spans[i][0], b, spans[i][2])
    for (a1, b1, n1), (a2, b2, n2) in zip(spans, spans[1:]):
        assert b1 < a2, f'overlap {n1} / {n2}'

    # write new file (verbatim spans, original indentation preserved)
    out = [f'// {spec["title"]} — moved verbatim from graph/tests/mod.rs (step 15).', '']
    out.append('use super::*;')
    for imp in spec['extra']:
        out.append(imp)
    out.append('')
    body_parts = []
    for s, e, name in spans:
        part = lines[s:e + 1]
        assert any(f'fn {name}(' in l for l in part), f'span missing fn line for {name}'
        body_parts.append('\n'.join(part))
    out.extend(body_parts)
    target = f'{ROOT}/{stem}.rs'
    open(target, 'w', encoding='utf-8').write('\n'.join(out).rstrip('\n') + '\n')

    # remove spans from mod.rs (bottom-up), collapse blank runs
    for s, e, name in sorted(spans, reverse=True):
        del lines[s:e + 1]
    collapsed = []
    for l in lines:
        if l.strip() == '' and collapsed and collapsed[-1].strip() == '':
            continue
        collapsed.append(l)

    # insert `mod stem;` after the last existing mod decl (or after imports block)
    mod_idx = None
    for i, l in enumerate(collapsed):
        if re.match(r'^\s*mod \w+;', l):
            mod_idx = i
    decl = f'    mod {stem};'
    if mod_idx is not None:
        collapsed.insert(mod_idx + 1, decl)
    else:
        # after the last leading import line
        ins = 0
        for i, l in enumerate(collapsed):
            st = l.strip()
            if st.startswith('use ') or st == '':
                ins = i + 1
            elif st.startswith('//'):
                continue
            else:
                break
        collapsed.insert(ins, decl)

    open(MOD, 'w', encoding='utf-8').write('\n'.join(collapsed).rstrip('\n') + '\n')
    print(f'{stem}: wrote {target} ({len(out)} lines), removed {sum(e - s + 1 for s, e, _ in spans)} lines from mod.rs')


if __name__ == '__main__':
    main()
