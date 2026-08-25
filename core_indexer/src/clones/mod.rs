// CodeRadar Stage 2 — token-level clone detection (Types 1–3).
//
// Three-layer funnel, mirroring fossil's detector design
// ("Merkle hashing, MinHash+LSH, and SimHash") with the layers we need now:
//
//   A  exact RAW-token hash   → Type-1 groups          O(n)
//   B  exact NORM-token hash  → Type-2 groups          O(n)
//   C  MinHash + banded LSH   → Type-3 candidates      O(n) + O(candidates)
//   verify candidates by true shingle-set Jaccard (APTED lands in Stage 6)
//
// Budget note: this orchestration stays < 300 LOC on purpose — fossil's
// equivalent sprawls across 900+. Fingerprint memoization is keyed by
// `Function.content_hash` so incremental runs re-tokenize only changed
// bodies; the store is an in-memory CACHE (plan §4 principle 9), never a
// second source of truth.

pub mod lsh_index;
pub mod minhash;
pub mod tokens;

use std::collections::HashMap;
use std::sync::Arc;

use crate::scoring::{clone_confidence, tier_of, Tier};
use crate::types::{ByteSpan, EntityId, Language, ProjectedGraph};

use self::lsh_index::LshIndex;
use self::minhash::MinHash;
use self::tokens::{tokenize_body, Mode};

const SHINGLE_K: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneType {
    Type1,
    Type2,
    Type3,
}

impl CloneType {
    pub fn as_str(self) -> &'static str {
        match self {
            CloneType::Type1 => "type-1",
            CloneType::Type2 => "type-2",
            CloneType::Type3 => "type-3",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CloneInstance {
    pub entity_id: EntityId,
    pub span: ByteSpan,
    /// File path the body lives in (for review links).
    pub file: String,
}

#[derive(Clone, Debug)]
pub struct CloneGroup {
    pub clone_type: CloneType,
    /// 1.0 for Types 1–2; true shingle Jaccard for Type-3.
    pub similarity: f64,
    pub instances: Vec<CloneInstance>,
    pub confidence_tier: Tier,
}

#[derive(Clone, Copy, Debug)]
pub struct CloneOptions {
    pub min_lines: usize,
    pub min_similarity: f64,
}

impl Default for CloneOptions {
    fn default() -> Self {
        Self { min_lines: 10, min_similarity: 0.8 }
    }
}

/// One fingerprinted function body.
struct Fp {
    entity_id: EntityId,
    file: String,
    span: ByteSpan,
    lines: usize,
    raw_hash: u64,
    norm_hash: u64,
    sig: MinHash,
    shingles: std::collections::HashSet<u64>,
}

fn xxh_tokens(tokens: &[u32]) -> u64 {
    let mut buf = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        buf.extend_from_slice(&t.to_le_bytes());
    }
    xxhash_rust::xxh3::xxh3_64(&buf)
}

/// Detect clone groups across every function in the projection.
/// Memoizes tokenization by `content_hash` within this run.
pub fn detect_clones(graph: &ProjectedGraph, options: CloneOptions) -> Vec<CloneGroup> {
    // Module sources read once each; language per module.
    let mut sources: HashMap<&EntityId, String> = HashMap::new();
    let mut languages: HashMap<&EntityId, Language> = HashMap::new();
    let mut paths: HashMap<&EntityId, String> = HashMap::new();
    for (mid, m) in &graph.modules {
        if let Ok(src) = std::fs::read_to_string(&m.path) {
            sources.insert(mid, src);
        }
        languages.insert(mid, m.language);
        paths.insert(mid, m.path.to_string_lossy().to_string());
    }

    // Fingerprint pass (memoized by content_hash within this run).
    let mut memo: HashMap<u64, (u64, u64, Arc<MinHash>, Arc<std::collections::HashSet<u64>>)> =
        HashMap::new();
    let mut fps: Vec<Fp> = Vec::new();
    let mut assigned: std::collections::HashSet<EntityId> = std::collections::HashSet::new();

    for (id, f) in &graph.functions {
        let lines = f.exit_line.saturating_sub(f.line).saturating_add(1);
        if lines < options.min_lines {
            continue;
        }
        let Some(src) = sources.get(&f.parent_module) else { continue };
        let Some(body) = src.get(f.body_span.start..f.body_span.end) else { continue };
        if body.trim().is_empty() {
            continue;
        }
        let lang = languages.get(&f.parent_module).copied().unwrap_or(Language::Python);

        let (raw_hash, norm_hash, sig, shingle_set) = match memo.get(&f.content_hash) {
            Some(hit) => hit.clone(),
            None => {
                let raw = tokenize_body(body, lang, Mode::Raw);
                let norm = tokenize_body(body, lang, Mode::Normalized);
                let rh = xxh_tokens(&raw);
                let nh = xxh_tokens(&norm);
                let shingle_set: std::collections::HashSet<u64> =
                    tokens::shingles(&norm, SHINGLE_K).collect();
                let sig = MinHash::of(shingle_set.iter().copied());
                let hit = (rh, nh, Arc::new(sig), Arc::new(shingle_set));
                memo.insert(f.content_hash, hit.clone());
                hit
            }
        };

        fps.push(Fp {
            entity_id: id.clone(),
            file: paths.get(&f.parent_module).cloned().unwrap_or_default(),
            span: f.body_span,
            lines,
            raw_hash,
            norm_hash,
            sig: (*sig).clone(),
            shingles: (*shingle_set).clone(),
        });
    }

    // ── Layer A: Type-1 groups ────────────────────────────────────────
    let mut groups: Vec<CloneGroup> = Vec::new();
    let mut by_raw: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, fp) in fps.iter().enumerate() {
        by_raw.entry(fp.raw_hash).or_default().push(i);
    }
    for (_, members) in by_raw {
        if members.len() < 2 {
            continue;
        }
        members.iter().for_each(|i| {
            assigned.insert(fps[*i].entity_id.clone());
        });
        groups.push(build_group(CloneType::Type1, 1.0, &fps, &members));
    }

    // ── Layer B: Type-2 groups over the remainder ────────────────────
    // Match against ALL bodies — a renamed copy of a Type-1 pair is itself
    // a Type-2 clone even though its partners were classified first.
    let mut by_norm: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, fp) in fps.iter().enumerate() {
        by_norm.entry(fp.norm_hash).or_default().push(i);
    }
    for (_, members) in by_norm {
        // A lone unassigned body whose normalized stream matches already-
        // classified partners is STILL a Type-2 clone of them — require
        // ≥ 2 total matches, not ≥ 2 unassigned.
        let unassigned: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !assigned.contains(&fps[*i].entity_id))
            .collect();
        if members.len() < 2 || unassigned.is_empty() {
            continue;
        }
        unassigned.iter().for_each(|i| {
            assigned.insert(fps[*i].entity_id.clone());
        });
        groups.push(build_group(CloneType::Type2, 1.0, &fps, &unassigned));
    }

    // ── Layer C: LSH candidates → verified Type-3 groups ─────────────
    let pool: Vec<usize> = fps
        .iter()
        .enumerate()
        .filter(|(_, fp)| !assigned.contains(&fp.entity_id))
        .map(|(i, _)| i)
        .collect();
    let mut lsh = LshIndex::new(16, 8);
    for (slot, i) in pool.iter().enumerate() {
        lsh.insert(slot as u32, fps[*i].sig.clone());
    }
    let slot_index = |slot: u32| pool[slot as usize];

    let mut uf: Vec<usize> = (0..pool.len()).collect();
    fn find(uf: &mut Vec<usize>, x: usize) -> usize {
        let mut x = x;
        while uf[x] != x {
            uf[x] = uf[uf[x]];
            x = uf[x];
        }
        x
    }
    for (a, b) in lsh.candidate_pairs() {
        // Union-find operates in SLOT space; fps indexes come after mapping.
        let sim = jaccard(&fps[slot_index(a)].shingles, &fps[slot_index(b)].shingles);
        if sim >= options.min_similarity {
            let ra = find(&mut uf, a as usize);
            let rb = find(&mut uf, b as usize);
            if ra != rb {
                uf[ra] = rb;
            }
        }
    }
    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in &pool {
        clusters.entry(find(&mut uf, i)).or_default().push(i);
    }
    for (_, members) in clusters {
        if members.len() < 2 {
            continue;
        }
        // Group similarity: average of pairwise true Jaccard.
        let mut sum = 0.0;
        let mut pairs = 0usize;
        for x in 0..members.len() {
            for y in x + 1..members.len() {
                sum += jaccard(&fps[members[x]].shingles, &fps[members[y]].shingles);
                pairs += 1;
            }
        }
        let sim = if pairs > 0 { sum / pairs as f64 } else { 0.0 };
        groups.push(build_group(CloneType::Type3, sim, &fps, &members));
    }

    groups.sort_by(|a, b| {
        b.instances
            .len()
            .cmp(&a.instances.len())
            .then(b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal))
    });
    groups
}

fn jaccard(a: &std::collections::HashSet<u64>, b: &std::collections::HashSet<u64>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    inter as f64 / union as f64
}

fn build_group(ty: CloneType, similarity: f64, fps: &[Fp], members: &[usize]) -> CloneGroup {
    let min_lines = members.iter().map(|&i| fps[i].lines).min().unwrap_or(0);
    CloneGroup {
        clone_type: ty,
        similarity,
        confidence_tier: tier_of(clone_confidence(similarity, min_lines)),
        instances: members
            .iter()
            .map(|&i| CloneInstance {
                entity_id: fps[i].entity_id.clone(),
                span: fps[i].span,
                file: fps[i].file.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Function, ParseQuality};

    #[test]
    fn normalization_makes_renames_equivalent() {
        let a = tokenize_body("def f(x):\n    return x + 1", Language::Python, Mode::Raw);
        let b = tokenize_body("def g(y):\n    return y + 2", Language::Python, Mode::Raw);
        let na = tokenize_body("def f(x):\n    return x + 1", Language::Python, Mode::Normalized);
        let nb = tokenize_body("def g(y):\n    return y + 2", Language::Python, Mode::Normalized);
        assert_ne!(a, b, "raw streams keep identifier text");
        assert_eq!(na, nb, "normalized streams collapse renames");
        // Comments never participate.
        let c = tokenize_body("def f(x):\n    # note\n    return x + 1", Language::Python, Mode::Raw);
        assert_eq!(a, c);
    }

    #[test]
    fn minhash_estimates_jaccard() {
        let s1: Vec<u64> = (0..200).map(|i| ((i as u64) * 2654435761u64)).collect();
        let s2: Vec<u64> = s1.iter().map(|v| v + 7).collect(); // fully disjoint
        let m1 = MinHash::of(s1.iter().copied());
        let m2 = MinHash::of(s2.iter().copied());
        let m3 = MinHash::of(s1.iter().copied()); // identical
        assert!(m1.estimate_jaccard(&m3) == 1.0);
        assert!(m1.estimate_jaccard(&m2) < 0.05);
    }

    #[test]
    fn lsh_collides_identical_signatures() {
        let mut idx = LshIndex::new(16, 8);
        let sig = MinHash::of(0..500);
        idx.insert(0, sig.clone());
        idx.insert(1, sig.clone());
        let pairs = idx.candidate_pairs();
        assert!(pairs.contains(&(0, 1)), "identical signatures must collide");
    }

    #[test]
    fn detect_finds_type1_and_type2_on_fixture_dir() {
        use crate::graph::deadcode;
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("dup.py");
        let src: &str = "def clone_a(x):
    total = 0
    for i in range(20):
        total += i * x
    return total

def clone_b(x):
    total = 0
    for i in range(20):
        total += i * x
    return total

def clone_c(y):
    acc = 0
    for j in range(99):
        acc += j * y
    return acc

def unrelated(q):
    return sorted({k: v for k, v in q.items()}, reverse=True)[0]
";
        std::fs::write(&src_path, src).unwrap();

        // Build the projection by hand: one module record pointing at the
        // file, four functions whose body spans cover their bodies.
        let mut g = crate::smells::engine::tests::empty_graph();
        let mut module = mk_module("dup.py::module", &src_path);
        module.language = Language::Python;
        g.modules.insert("dup.py::module".into(), Arc::new(module));

        // body_span covers the BODY ONLY (after the signature line) —
        // matching real extraction, where spans start at the first body
        // token. Including the def line would make same-body clones differ
        // by their function name in RAW mode.
        let span_of = |name: &str| {
            let start = src.find(&format!("def {name}")).unwrap();
            let colon = src[start..].find(":\n").map(|rel| start + rel + 2).unwrap();
            let end = src[colon..]
                .find("\n\ndef ")
                .map(|rel| colon + rel)
                .unwrap_or(src.len());
            ByteSpan { start: colon, end }
        };
        let lines_of = |name: &str| src[span_of(name).start..span_of(name).end].lines().count();
        for name in ["clone_a", "clone_b", "clone_c", "unrelated"] {
            let mut f = func_fp(name);
            f.body_span = span_of(name);
            f.line = 1;
            f.exit_line = f.line + lines_of(name) - 1;
            f.content_hash = xxhash_rust::xxh3::xxh3_64(name.as_bytes());
            g.functions.insert(format!("dup.py::{name}"), Arc::new(f));
        }

        let groups = detect_clones(&g, CloneOptions { min_lines: 4, min_similarity: 0.7 });

        let type_of = |name: &str| {
            groups
                .iter()
                .find(|grp| {
                    grp.instances
                        .iter()
                        .any(|inst| inst.entity_id.contains(&format!("::{name}")))
                })
                .map(|grp| grp.clone_type)
        };
        assert_eq!(
            type_of("clone_a"),
            Some(CloneType::Type1),
            "identical bodies form a Type-1 group"
        );
        assert_eq!(type_of("clone_b"), Some(CloneType::Type1));
        assert_eq!(
            type_of("clone_c"),
            Some(CloneType::Type2),
            "renamed body lands in a Type-2 group"
        );
        assert!(type_of("unrelated").is_none(), "dissimilar bodies must not be grouped");
    }

    fn func_fp(name: &str) -> Function {
        Function {
            id: format!("dup.py::{name}"),
            name: name.into(),
            parent_module: "dup.py::module".into(),
            parent_class: None,
            parameters: vec![],
            return_type: None,
            calls: vec![],
            resolved_calls: vec![],
            decorators: vec![],
            setter_of: None,
            line: 1,
            exit_line: 5,
            docstring: None,
            kind: crate::types::FunctionKind::Free,
            is_async: false,
            is_generator: false,
            source: crate::types::SourceType::Impl,
            signature_hash: 0,
            body_hash: 0,
            metrics: Default::default(),
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            content_hash: 0,
            span: ByteSpan { start: 0, end: 10 },
            name_span: ByteSpan { start: 0, end: 1 },
            params_span: ByteSpan { start: 0, end: 1 },
            body_span: ByteSpan { start: 0, end: 10 },
            decorators_span: None,
            embedding: crate::types::EmbeddingVec { vec: vec![], hash: String::new() },
        }
    }

    fn mk_module(id: &str, path: &std::path::Path) -> crate::types::Module {
        crate::types::Module {
            id: id.into(),
            name: id.into(),
            path: path.to_path_buf(),
            language: Language::Python,
            package: None,
            exports: vec![],
            star_exports: None,
            classes: vec![],
            functions: vec![],
            imports: vec![],
            constants: vec![],
            type_aliases: vec![],
            parse_quality: ParseQuality::Clean,
            file_version: 1,
            content_hash: 0,
            embedding: crate::types::EmbeddingVec { vec: vec![], hash: String::new() },
        }
    }
}
