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
//! yields empty maps, which reproduce the old convention/suffix behaviour
//! exactly. Only `<root>/`-level configs are read; nested monorepo
//! configs are a documented gap (nearest-config-wins is future work).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One tsconfig `paths` entry: an exact specifier or a `prefix/*` pattern.
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

/// Parsed project path maps. Empty = no configs found (old behaviour).
#[derive(Clone, Debug, Default)]
pub struct PathMaps {
    /// tsconfig `paths`, exacts first then longest-prefix-first.
    /// Deterministic: rebuilt from a BTreeMap, ties broken by key order.
    pub ts_aliases: Vec<TsAlias>,
    /// tsconfig `baseUrl`, root-relative (`None` = unset).
    pub ts_base_url: Option<String>,
    /// Python src-layout roots, root-relative (`["src"]`).
    pub python_roots: Vec<String>,
}

impl PathMaps {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.ts_aliases.is_empty() && self.ts_base_url.is_none() && self.python_roots.is_empty()
    }

    /// Read `<root>/tsconfig.json` and `<root>/pyproject.toml`.
    /// Never errors; anything unreadable yields empty maps.
    pub fn discover(root: &str) -> Self {
        let root = Path::new(root);
        let mut maps = Self::new();
        maps.read_tsconfig(&root.join("tsconfig.json"), 0, &mut Vec::new());
        maps.read_pyproject(&root.join("pyproject.toml"), root);
        maps
    }

    // ── tsconfig ────────────────────────────────────────────────

    fn read_tsconfig(&mut self, path: &Path, depth: usize, visited: &mut Vec<PathBuf>) {
        if depth > 8 {
            return;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if visited.contains(&canonical) {
            return;
        }
        visited.push(canonical);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return,
        };
        // tsconfig allows comments and trailing commas; serde_json does
        // not. Strip `//` and `/* */` comments before parsing — a
        // best-effort pass (a `//` inside a string literal would corrupt,
        // so bail to empty maps if parsing still fails).
        let value: serde_json::Value = match serde_json::from_str(&strip_json_comments(&text)) {
            Ok(v) => v,
            Err(_) => return,
        };
        // `extends` first: the child's entries override the parent's.
        if let Some(base) = value.get("extends").and_then(|e| e.as_str()) {
            let base_file = if base.ends_with(".json") {
                base.to_string()
            } else {
                format!("{base}.json")
            };
            let base_path = path.parent().unwrap_or(Path::new(".")).join(base_file);
            self.read_tsconfig(&base_path, depth + 1, visited);
        }
        let Some(opts) = value.get("compilerOptions") else {
            return;
        };
        if self.ts_base_url.is_none() {
            if let Some(base) = opts.get("baseUrl").and_then(|b| b.as_str()) {
                // baseUrl "." is the project root itself — same as unset
                // (paths are already root-relative); skip it so queries test
                // `is_some` for a real anchor.
                let trimmed = base.trim_matches('/');
                if !trimmed.is_empty() && trimmed != "." {
                    self.ts_base_url = Some(trimmed.to_string());
                }
            }
        }
        if let Some(paths) = opts.get("paths").and_then(|p| p.as_object()) {
            // Child keys override inherited ones (map merge, not replace).
            let mut merged: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for a in &self.ts_aliases {
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
                            .map(|t| t.trim_matches('/').to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                merged.insert(k.clone(), targets);
            }
            self.ts_aliases = merged
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
            self.ts_aliases.sort_by(|a, b| {
                b.exact
                    .cmp(&a.exact)
                    .then_with(|| b.from.len().cmp(&a.from.len()))
                    .then_with(|| a.from.cmp(&b.from))
            });
        }
    }

    // ── pyproject ───────────────────────────────────────────────

    fn read_pyproject(&mut self, path: &Path, root: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return,
        };
        let value: toml::Value = match text.parse() {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut roots: Vec<String> = Vec::new();
        // setuptools: [tool.setuptools.packages.find] where = ["src"]
        if let Some(where_) = value
            .get("tool")
            .and_then(|t| t.get("setuptools"))
            .and_then(|s| s.get("packages"))
            .and_then(|p| p.get("find"))
            .and_then(|f| f.get("where"))
            .and_then(|w| w.as_array())
        {
            roots.extend(where_.iter().filter_map(|r| r.as_str()).map(str::to_string));
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
                    roots.push(dir.to_string());
                } else if let Some(stripped) = dir.strip_suffix(pkg.as_str()) {
                    if let Some(prefix) = stripped.strip_suffix('/').filter(|p| !p.is_empty()) {
                        // package-dir = {foo = "src/foo"} ⇒ root "src".
                        roots.push(prefix.to_string());
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
                    roots.push(from.to_string());
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
                        roots.push(parent.to_string());
                    }
                }
            }
        }
        // Keep root-relative, sorted, deduplicated. "." is a no-op
        // (suffix matching already covers the root) — drop it. Reject
        // escapes (`..`) and confirm the dir exists under root so a
        // stale config cannot invent resolution targets.
        let mut seen = std::collections::HashSet::new();
        for r in roots {
            let r = r.trim_matches('/').to_string();
            if r.is_empty()
                || r == "."
                || r.starts_with("..")
                || r.contains("/../")
                || !seen.insert(r.clone())
            {
                continue;
            }
            if root.join(&r).is_dir() {
                self.python_roots.push(r);
            }
        }
        self.python_roots.sort();
    }
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
        let maps = PathMaps::discover(dir.to_str().unwrap());
        assert_eq!(maps.ts_base_url.as_deref(), Some("src"));
        assert_eq!(maps.ts_aliases.len(), 1);
        assert!(!maps.ts_aliases[0].exact);
        assert_eq!(maps.ts_aliases[0].from, "@utils/");
        assert_eq!(maps.ts_aliases[0].to, vec!["lib/utils/*", "src/utils/*"]);
        assert!(maps.python_roots.is_empty());
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
        let maps = PathMaps::discover(dir.to_str().unwrap());
        // Child baseUrl absent and parent's "." means root ⇒ unset;
        // child @app wins over the parent's.
        assert!(maps.ts_base_url.is_none());
        assert!(maps
            .ts_aliases
            .iter()
            .any(|a| a.exact && a.from == "@app" && a.to == vec!["src/app"]));
        assert!(maps
            .ts_aliases
            .iter()
            .any(|a| !a.exact && a.from == "@lib/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_discover_missing_configs_is_empty() {
        let dir = write_tmp("empty", &[("a.py", "x")]);
        let maps = PathMaps::discover(dir.to_str().unwrap());
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
        let maps = PathMaps::discover(dir.to_str().unwrap());
        assert_eq!(maps.python_roots, vec!["src"]);
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
        let maps = PathMaps::discover(dir.to_str().unwrap());
        assert_eq!(maps.python_roots, vec!["src"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
