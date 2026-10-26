//! Project path-map discovery (§2.5).
//!
//! `find_module_by_dotted_name` used to normalize TS `@/...` and `~/...`
//! to `src/...` by convention. Projects with a non-conventional map
//! resolved by suffix match or not at all. This module reads the real
//! maps once per analyze/load — `<root>/tsconfig.json`
//! (`compilerOptions.baseUrl` + `compilerOptions.paths`, following
//! `extends` chains) and `<root>/pyproject.toml` (src-layout roots) —
//! and stores them on the projection for the resolver to consult.
//!
//! Discovery never fails: a missing, unreadable, or unparseable config
//! yields empty scopes, which reproduce the old convention/suffix
//! behaviour exactly. Every config file found by the analyzed-tree walk
//! (same ignore rules as indexing, `MAX_CONFIGS` cap) becomes a scope
//! anchored to its own directory; the nearest scope owning the importer
//! wins (root scope always exists as the fallback).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One tsconfig `paths` entry: an exact specifier or a `prefix/*` pattern.
/// Targets are stored **root-relative** (anchored to the config file's
/// own directory at parse time, §2.5 slice B), so nested package configs
/// never leak their relative targets into the wrong subtree.
#[derive(Clone, Debug, Default)]
pub struct TsAlias {
    /// Specifier prefix before the `*` (`""` for a bare `*` key), or the
    /// full key when `exact`.
    pub from: String,
    /// True when the key has no `*`: only the whole specifier matches.
    pub exact: bool,
    /// Target prefixes in tsconfig array order (first hit wins).
    pub to: Vec<String>,
}

/// One config file's contribution: the directory that owns it plus the
/// maps parsed from it (targets already anchored to the project root).
/// `dir == ""` is the project-root scope.
#[derive(Clone, Debug, Default)]
pub struct ConfigScope {
    /// Config directory, root-relative (`""` = project root).
    pub dir: String,
    /// tsconfig `paths`, exacts first then longest-prefix-first.
    /// Deterministic: rebuilt from a BTreeMap, ties broken by key order.
    pub ts_aliases: Vec<TsAlias>,
    /// tsconfig `baseUrl`, root-relative (`None` = unset).
    pub ts_base_url: Option<String>,
    /// Python src-layout roots, root-relative (`["src"]`).
    pub python_roots: Vec<String>,
}

impl ConfigScope {
    pub fn is_empty(&self) -> bool {
        self.ts_aliases.is_empty() && self.ts_base_url.is_none() && self.python_roots.is_empty()
    }
}

/// Parsed project path maps: one scope per config file found under the
/// root (the root scope always exists, possibly empty). Empty scopes are
/// kept — they mark "a config exists here with no maps", which still
/// wins nearest-scope selection over an ancestor's aliases (TS: the
/// nearest config is THE config).
#[derive(Clone, Debug)]
pub struct PathMaps {
    /// One scope per config file found (root scope first, `dir == ""`).
    pub scopes: Vec<ConfigScope>,
}

impl Default for PathMaps {
    /// The root scope always exists, so `scope_for` never fails — even
    /// on hand-built projections that never ran `discover` (unit tests).
    fn default() -> Self {
        Self {
            scopes: vec![ConfigScope::default()],
        }
    }
}

/// Hard cap on discovered configs: bounds the walk on pathological
/// trees. Shallowest wins (root configs matter most); deterministic.
const MAX_CONFIGS: usize = 64;

impl PathMaps {
    pub fn new() -> Self {
        Self::default()
    }

    /// A root scope always exists (possibly empty), so `default()` is a
    /// well-formed empty map set; `discover` fills it in.
    pub fn is_empty(&self) -> bool {
        self.scopes.iter().all(|s| s.is_empty())
    }

    /// Nearest scope for an importer directory (root-relative, `""` =
    /// root): the deepest config dir that is a path-prefix of the
    /// importer. The root scope always matches, so this never fails.
    /// TS semantics: the nearest config is THE config — no fallback to
    /// ancestor scopes on a miss (the generic suffix fallback still
    /// applies downstream).
    pub fn scope_for(&self, importer_dir: &str) -> &ConfigScope {
        let mut best = &self.scopes[0];
        for scope in &self.scopes {
            if scope.dir.is_empty() {
                continue;
            }
            let prefix = format!("{}/", scope.dir);
            if (importer_dir == scope.dir || importer_dir.starts_with(&prefix))
                && scope.dir.len() > best.dir.len()
            {
                best = scope;
            }
        }
        best
    }

    /// Walk the analyzed tree (same ignore rules as indexing) and parse
    /// every `tsconfig.json` / `pyproject.toml` found, each anchored to
    /// its own directory (§2.5 slice B). Never errors; anything
    /// unreadable yields empty scopes. At most MAX_CONFIGS files —
    /// shallowest first, deterministic.
    pub fn discover(root: &str, project: &super::config::ProjectConfig, extra: &[String]) -> Self {
        let root_path = Path::new(root);
        let root_canon = root_path
            .canonicalize()
            .unwrap_or_else(|_| root_path.to_path_buf());
        let mut rels: Vec<String> = Vec::new();
        for entry in crate::project_walk(root, project, extra) {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy();
            if name != "tsconfig.json" && name != "pyproject.toml" {
                continue;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let canon = entry
                .path()
                .canonicalize()
                .unwrap_or_else(|_| entry.path().to_path_buf());
            let Ok(rel) = canon.strip_prefix(&root_canon) else {
                continue;
            };
            rels.push(rel.to_string_lossy().replace('\\', "/"));
        }
        rels.sort_by_key(|r| (r.matches('/').count(), r.clone()));
        rels.truncate(MAX_CONFIGS);
        // Group by config directory: a dir holding both files yields one
        // scope. Root scope first, then lexical (deterministic).
        let mut dirs: Vec<String> = rels
            .iter()
            .map(|r| parent_dir(r).to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        dirs.sort();
        let mut scopes = Vec::with_capacity(dirs.len().max(1));
        // The root scope always exists, even with no root config.
        scopes.push(ConfigScope {
            dir: String::new(),
            ..ConfigScope::default()
        });
        for dir in dirs {
            if dir.is_empty() {
                continue;
            }
            scopes.push(ConfigScope {
                dir,
                ..ConfigScope::default()
            });
        }
        for scope in &mut scopes {
            let base = root_canon.join(&scope.dir);
            let ts_path = base.join("tsconfig.json");
            if ts_path.is_file() {
                let anchor = scope.dir.clone();
                *scope =
                    Self::scope_from_tsconfig(&ts_path, &anchor, &root_canon, 0, &mut Vec::new());
            }
            let py_path = base.join("pyproject.toml");
            if py_path.is_file() {
                scope.python_roots = Self::roots_from_pyproject(&py_path, &scope.dir, &root_canon);
            }
        }
        Self { scopes }
    }

    // ── tsconfig ────────────────────────────────────────────────

    fn scope_from_tsconfig(
        path: &Path,
        anchor: &str,
        root_canon: &Path,
        depth: usize,
        visited: &mut Vec<PathBuf>,
    ) -> ConfigScope {
        let mut scope = ConfigScope {
            dir: anchor.to_string(),
            ..ConfigScope::default()
        };
        if depth > 8 {
            return scope;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if visited.contains(&canonical) {
            return scope;
        }
        visited.push(canonical);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return scope,
        };
        // tsconfig allows comments and trailing commas; serde_json does
        // not. Strip `//` and `/* */` comments before parsing — a
        // best-effort pass (a `//` inside a string literal would corrupt,
        // so bail to empty maps if parsing still fails).
        let value: serde_json::Value = match serde_json::from_str(&strip_json_comments(&text)) {
            Ok(v) => v,
            Err(_) => return scope,
        };
        // `extends` first: the child's entries override the parent's.
        // The parent's anchor is ITS directory, not ours.
        if let Some(base) = value.get("extends").and_then(|e| e.as_str()) {
            let base_file = if base.ends_with(".json") {
                base.to_string()
            } else {
                format!("{base}.json")
            };
            let base_path = path.parent().unwrap_or(Path::new(".")).join(base_file);
            let parent_anchor = base_path
                .canonicalize()
                .ok()
                .and_then(|c| c.strip_prefix(root_canon).ok().map(|r| r.to_path_buf()))
                .and_then(|r| r.parent().map(|p| p.to_path_buf()))
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let parent = Self::scope_from_tsconfig(
                &base_path,
                &parent_anchor,
                root_canon,
                depth + 1,
                visited,
            );
            scope.ts_aliases = parent.ts_aliases;
            scope.ts_base_url = parent.ts_base_url;
        }
        let Some(opts) = value.get("compilerOptions") else {
            return scope;
        };
        if scope.ts_base_url.is_none() {
            if let Some(base) = opts.get("baseUrl").and_then(|b| b.as_str()) {
                // baseUrl is relative to the config's own dir: anchor it.
                // "." means the config dir itself; unset stays unset
                // only at the root (queries test `is_some`).
                let trimmed = base.trim_matches('/');
                if trimmed == "." {
                    if !anchor.is_empty() {
                        scope.ts_base_url = Some(anchor.to_string());
                    }
                } else if !trimmed.is_empty() {
                    if let Some(anchored) = join_anchor(anchor, trimmed) {
                        scope.ts_base_url = Some(anchored);
                    }
                }
            }
        }
        if let Some(paths) = opts.get("paths").and_then(|p| p.as_object()) {
            // Child keys override inherited ones (map merge, not replace).
            // Every target is anchored to THIS file's dir as it is read —
            // inherited entries already carry their own file's anchor.
            let mut merged: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for a in &scope.ts_aliases {
                merged.insert(
                    if a.exact {
                        a.from.clone()
                    } else {
                        format!("{}*", a.from)
                    },
                    a.to.clone(),
                );
            }
            for (k, v) in paths {
                let targets: Vec<String> = v
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str())
                            .map(|t| t.trim_matches('/'))
                            .filter_map(|t| join_anchor(anchor, t))
                            .collect()
                    })
                    .unwrap_or_default();
                merged.insert(k.clone(), targets);
            }
            scope.ts_aliases = merged
                .into_iter()
                .map(|(k, to)| {
                    if let Some(prefix) = k.strip_suffix('*') {
                        TsAlias {
                            from: prefix.to_string(),
                            exact: false,
                            to,
                        }
                    } else {
                        TsAlias {
                            from: k,
                            exact: true,
                            to,
                        }
                    }
                })
                .collect();
            // Exacts first, then longest prefix — deterministic.
            scope.ts_aliases.sort_by(|a, b| {
                b.exact
                    .cmp(&a.exact)
                    .then_with(|| b.from.len().cmp(&a.from.len()))
                    .then_with(|| a.from.cmp(&b.from))
            });
        }
        scope
    }

    // ── pyproject ───────────────────────────────────────────────

    /// Parse one pyproject into root-anchored src-layout roots.
    /// `anchor` is the config file's own dir; targets join onto it.
    fn roots_from_pyproject(path: &Path, anchor: &str, root_canon: &Path) -> Vec<String> {
        let mut roots: Vec<String> = Vec::new();
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return roots,
        };
        let value: toml::Value = match text.parse() {
            Ok(v) => v,
            Err(_) => return roots,
        };
        let mut raw: Vec<String> = Vec::new();
        // setuptools: [tool.setuptools.packages.find] where = ["src"]
        if let Some(where_) = value
            .get("tool")
            .and_then(|t| t.get("setuptools"))
            .and_then(|s| s.get("packages"))
            .and_then(|p| p.get("find"))
            .and_then(|f| f.get("where"))
            .and_then(|w| w.as_array())
        {
            raw.extend(where_.iter().filter_map(|r| r.as_str()).map(str::to_string));
        }
        // setuptools: [tool.setuptools] package-dir = {"" = "src"}
        if let Some(dir) = value
            .get("tool")
            .and_then(|t| t.get("setuptools"))
            .and_then(|s| s.get("package-dir"))
            .and_then(|d| d.as_table())
        {
            for (pkg, dir) in dir {
                let Some(dir) = dir.as_str() else { continue };
                if pkg.is_empty() {
                    raw.push(dir.to_string());
                } else if let Some(stripped) = dir.strip_suffix(pkg.as_str()) {
                    if let Some(prefix) = stripped.strip_suffix('/').filter(|p| !p.is_empty()) {
                        // package-dir = {foo = "src/foo"} ⇒ root "src".
                        raw.push(prefix.to_string());
                    }
                }
            }
        }
        // poetry: [[tool.poetry.packages]] {include, from = "src"}
        if let Some(pkgs) = value
            .get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("packages"))
            .and_then(|p| p.as_array())
        {
            for pkg in pkgs {
                if let Some(from) = pkg.get("from").and_then(|f| f.as_str()) {
                    raw.push(from.to_string());
                }
            }
        }
        // hatch: [tool.hatch.build.targets.wheel] packages = ["src/foo"]
        if let Some(pkgs) = value
            .get("tool")
            .and_then(|t| t.get("hatch"))
            .and_then(|h| h.get("build"))
            .and_then(|b| b.get("targets"))
            .and_then(|t| t.get("wheel"))
            .and_then(|w| w.get("packages"))
            .and_then(|p| p.as_array())
        {
            for pkg in pkgs {
                if let Some(p) = pkg.as_str() {
                    if let Some((parent, _)) = p.rsplit_once('/') {
                        raw.push(parent.to_string());
                    }
                }
            }
        }
        // Anchor every root to the config's own dir, keep root-relative,
        // sorted, deduplicated. "." is a no-op (suffix matching already
        // covers the anchor dir itself) — drop it. Reject escapes and
        // confirm the dir exists under root so a stale config cannot
        // invent resolution targets.
        let mut seen = std::collections::HashSet::new();
        for r in raw {
            let Some(anchored) = join_anchor(anchor, r.trim_matches('/')) else {
                continue;
            };
            if anchored.is_empty()
                || anchored == "."
                || anchored.starts_with("..")
                || anchored.contains("/../")
                || !seen.insert(anchored.clone())
            {
                continue;
            }
            if root_canon.join(&anchored).is_dir() {
                roots.push(anchored);
            }
        }
        roots.sort();
        roots
    }
}

/// Parent directory of a slash-separated relative path (`""` for a
/// top-level file). No filesystem access — lexical only.
fn parent_dir(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(p, _)| p)
}

/// Join a config anchor dir with a config-relative target and normalize
/// lexically (`a/../b` → `b`). Returns `None` when the target escapes
/// above the project root (stale/malicious configs cannot invent
/// out-of-tree targets). Empty results (`"."` at the root) are `Some("")`.
fn join_anchor(anchor: &str, target: &str) -> Option<String> {
    if target.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if !anchor.is_empty() {
        parts.extend(anchor.split('/'));
    }
    for comp in target.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// Strip `//` line comments and `/* */` block comments outside string
/// literals. Best-effort for tsconfig's JSONC; parse failure after
/// stripping still yields empty maps (never corrupt).
fn strip_json_comments(text: &str) -> String {
    let v: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    let mut escaped = false;
    while i < v.len() {
        let c = v[i];
        i += 1;
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if i < v.len() && v[i] == '/' => {
                while i < v.len() && v[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i < v.len() && v[i] == '*' => {
                i += 1;
                while i + 1 < v.len() && !(v[i] == '*' && v[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discover_tmp(dir: &std::path::Path) -> PathMaps {
        PathMaps::discover(
            dir.to_str().unwrap(),
            &super::super::config::ProjectConfig::default(),
            &[],
        )
    }

    fn root_scope(maps: &PathMaps) -> &ConfigScope {
        maps.scopes
            .iter()
            .find(|s| s.dir.is_empty())
            .expect("root scope always exists")
    }

    fn write_tmp(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coderadar_pathmaps_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (rel, content) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        dir
    }

    #[test]
    fn test_discover_tsconfig_paths_and_base_url() {
        let dir = write_tmp(
            "ts",
            &[
                (
                    "tsconfig.json",
                    r#"{"compilerOptions": {"baseUrl": "src", "paths": {"@utils/*": ["lib/utils/*", "src/utils/*"]}}}"#,
                ),
                ("lib/utils/a.ts", "x"),
            ],
        );
        let maps = discover_tmp(&dir);
        assert_eq!(root_scope(&maps).ts_base_url.as_deref(), Some("src"));
        assert_eq!(root_scope(&maps).ts_aliases.len(), 1);
        assert!(!root_scope(&maps).ts_aliases[0].exact);
        assert_eq!(root_scope(&maps).ts_aliases[0].from, "@utils/");
        assert_eq!(
            root_scope(&maps).ts_aliases[0].to,
            vec!["lib/utils/*", "src/utils/*"]
        );
        assert!(root_scope(&maps).python_roots.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_discover_tsconfig_extends_and_comments() {
        let dir = write_tmp(
            "extends",
            &[
                (
                    "tsconfig.json",
                    r#"// project config
{
  "extends": "./base.json", /* merged */
  "compilerOptions": {"paths": {"@app": ["src/app"]}}
}"#,
                ),
                (
                    "base.json",
                    r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@lib/*": ["shared/*"], "@app": ["old/app"]}}}"#,
                ),
            ],
        );
        let maps = discover_tmp(&dir);
        // Child baseUrl absent and parent's "." means root ⇒ unset;
        // child @app wins over the parent's.
        assert!(root_scope(&maps).ts_base_url.is_none());
        assert!(root_scope(&maps)
            .ts_aliases
            .iter()
            .any(|a| a.exact && a.from == "@app" && a.to == vec!["src/app"]));
        assert!(root_scope(&maps)
            .ts_aliases
            .iter()
            .any(|a| !a.exact && a.from == "@lib/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_discover_missing_configs_is_empty() {
        let dir = write_tmp("empty", &[("a.py", "x")]);
        let maps = discover_tmp(&dir);
        assert!(maps.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_discover_pyproject_setuptools_src() {
        let dir = write_tmp(
            "py",
            &[
                (
                    "pyproject.toml",
                    "[tool.setuptools.packages.find]\nwhere = [\"src\"]\n",
                ),
                ("src/mylib/__init__.py", "x"),
            ],
        );
        let maps = discover_tmp(&dir);
        assert_eq!(root_scope(&maps).python_roots, vec!["src"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_discover_pyproject_ignores_missing_dirs() {
        let dir = write_tmp(
            "stale",
            &[
                (
                    "pyproject.toml",
                    "[tool.setuptools.packages.find]\nwhere = [\"src\", \"nope\"]\n",
                ),
                ("src/a/__init__.py", "x"),
            ],
        );
        let maps = discover_tmp(&dir);
        assert_eq!(root_scope(&maps).python_roots, vec!["src"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_discover_nested_scopes_anchored() {
        // §2.5 slice B: each nested config anchors to its own dir; the
        // nearest scope wins at query time.
        let dir = write_tmp(
            "nested",
            &[
                (
                    "tsconfig.json",
                    r#"{"compilerOptions": {"paths": {"@root/*": ["shared/*"]}}}"#,
                ),
                (
                    "packages/web/tsconfig.json",
                    r#"{"compilerOptions": {"baseUrl": "src", "paths": {"@/*": ["src/*"]}}}"#,
                ),
                (
                    "packages/shared/tsconfig.json",
                    r#"{"compilerOptions": {"paths": {"@shared/*": ["lib/*"]}}}"#,
                ),
                (
                    "packages/pkg-a/pyproject.toml",
                    "[tool.setuptools.packages.find]\nwhere = [\"src\"]\n",
                ),
                ("packages/pkg-a/src/m/__init__.py", "x"),
            ],
        );
        let maps = discover_tmp(&dir);
        assert_eq!(maps.scopes.len(), 4);
        assert!(maps.scopes[0].dir.is_empty());
        let web = maps
            .scopes
            .iter()
            .find(|s| s.dir == "packages/web")
            .unwrap();
        assert_eq!(web.ts_aliases[0].from, "@/");
        assert_eq!(web.ts_aliases[0].to, vec!["packages/web/src/*"]);
        assert_eq!(web.ts_base_url.as_deref(), Some("packages/web/src"));
        let shared = maps
            .scopes
            .iter()
            .find(|s| s.dir == "packages/shared")
            .unwrap();
        assert_eq!(shared.ts_aliases[0].to, vec!["packages/shared/lib/*"]);
        let pkg = maps
            .scopes
            .iter()
            .find(|s| s.dir == "packages/pkg-a")
            .unwrap();
        assert_eq!(pkg.python_roots, vec!["packages/pkg-a/src"]);
        // Nearest-wins selection.
        assert_eq!(maps.scope_for("packages/web/src").dir, "packages/web");
        assert_eq!(maps.scope_for("packages/web/deep/nest").dir, "packages/web");
        assert_eq!(maps.scope_for("packages/shared").dir, "packages/shared");
        assert_eq!(maps.scope_for("other").dir, "");
        assert_eq!(maps.scope_for("").dir, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_discover_extends_across_dirs_anchors_parent() {
        // §2.5 slice B: inherited targets stay anchored to the PARENT
        // config's dir — they must not drift into the child's subtree.
        let dir = write_tmp(
            "xextends",
            &[
                (
                    "tsconfig.base.json",
                    r#"{"compilerOptions": {"baseUrl": "shared", "paths": {"@lib/*": ["shared/*"]}}}"#,
                ),
                (
                    "packages/web/tsconfig.json",
                    r#"{"extends": "../../tsconfig.base.json", "compilerOptions": {"paths": {"@/*": ["src/*"]}}}"#,
                ),
            ],
        );
        let maps = discover_tmp(&dir);
        let web = maps
            .scopes
            .iter()
            .find(|s| s.dir == "packages/web")
            .unwrap();
        // Inherited entry: root-anchored, not packages/web-anchored.
        assert!(web
            .ts_aliases
            .iter()
            .any(|a| !a.exact && a.from == "@lib/" && a.to == vec!["shared/*"]));
        // Own entry anchored to the child dir; parent baseUrl inherits
        // root-anchored ("shared"), not re-anchored to the child.
        assert!(web
            .ts_aliases
            .iter()
            .any(|a| !a.exact && a.from == "@/" && a.to == vec!["packages/web/src/*"]));
        assert_eq!(web.ts_base_url.as_deref(), Some("shared"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
