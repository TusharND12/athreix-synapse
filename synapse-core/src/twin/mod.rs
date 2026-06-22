//! Project Digital Twin (Feature 3). Builds a file-level dependency graph by
//! scanning import/require/use statements across the project and resolving them
//! to local files. Nodes are files (classified by role), edges are imports.
//!
//! This is the import-scan engine — language-agnostic and dependency-light. A
//! future tree-sitter pass would add symbol-level nodes; the graph shape here
//! (nodes + edges) is forward-compatible. See docs/02-event-model.md.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::explain::extract_dependencies;
use crate::watcher::is_ignored;

const MAX_NODES: usize = 400;
const SOURCE_EXTS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go", "java", "rb", "vue", "svelte",
];
const RESOLVE_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py"];

#[derive(Serialize)]
pub struct TwinNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: String,
    pub degree: u32,
}

#[derive(Serialize)]
pub struct TwinEdge {
    pub source: String,
    pub target: String,
}

#[derive(Serialize)]
pub struct TwinGraph {
    pub nodes: Vec<TwinNode>,
    pub edges: Vec<TwinEdge>,
}

fn ext_of(path: &str) -> &str {
    path.rsplit_once('.').map(|(_, e)| e).unwrap_or("")
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Classify a file's architectural role from its path/name.
pub fn classify(path: &str) -> &'static str {
    let p = path.to_lowercase();
    let name = file_name(&p);
    if p.contains("/pages/") || name == "page.tsx" || name == "page.jsx" || name == "route.ts" {
        "page"
    } else if name.starts_with("use") && (ext_of(&p) == "ts" || ext_of(&p) == "tsx") {
        "hook"
    } else if p.contains("/hooks/") {
        "hook"
    } else if p.contains("/api/") || p.contains("/server/") || p.contains("route") {
        "api"
    } else if p.contains("/services/") || p.contains("service") {
        "service"
    } else if p.contains("/models/") || p.contains("schema") || p.contains("model") {
        "model"
    } else if p.contains("/components/") || ext_of(&p) == "tsx" || ext_of(&p) == "jsx" || ext_of(&p) == "vue" || ext_of(&p) == "svelte" {
        "component"
    } else if ext_of(&p) == "rs" {
        "rust"
    } else {
        "module"
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() > MAX_NODES * 2 {
            return;
        }
        let path = entry.path();
        if is_ignored(&path, root) {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => collect(root, &path, out),
            Ok(ft) if ft.is_file() => out.push(path),
            _ => {}
        }
    }
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Normalize a relative import joined onto the importer's directory.
fn join_norm(dir: &str, dep: &str) -> String {
    let mut stack: Vec<&str> = if dir.is_empty() {
        vec![]
    } else {
        dir.split('/').collect()
    };
    for seg in dep.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    stack.join("/")
}

/// Resolve an import specifier to a local project file (rel path), if possible.
fn resolve(importer: &str, dep: &str, files: &HashSet<String>) -> Option<String> {
    let base = if let Some(stripped) = dep.strip_prefix("@/") {
        // Common Next.js alias → src/
        format!("src/{stripped}")
    } else if dep.starts_with('.') {
        let dir = importer.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        join_norm(dir, dep)
    } else {
        return None; // external / bare specifier
    };

    if files.contains(&base) {
        return Some(base);
    }
    for e in RESOLVE_EXTS {
        let cand = format!("{base}.{e}");
        if files.contains(&cand) {
            return Some(cand);
        }
    }
    for e in RESOLVE_EXTS {
        let cand = format!("{base}/index.{e}");
        if files.contains(&cand) {
            return Some(cand);
        }
    }
    None
}

/// Build the dependency graph for a project root.
pub fn build(root: &Path) -> TwinGraph {
    let mut paths = Vec::new();
    collect(root, root, &mut paths);

    // Index of relative source-file paths.
    let rels: Vec<String> = paths
        .iter()
        .map(|p| rel(root, p))
        .filter(|r| SOURCE_EXTS.contains(&ext_of(r)))
        .take(MAX_NODES)
        .collect();
    let file_set: HashSet<String> = rels.iter().cloned().collect();

    let mut edges: Vec<TwinEdge> = Vec::new();
    let mut seen_edge: HashSet<(String, String)> = HashSet::new();
    let mut degree: HashMap<String, u32> = HashMap::new();

    for r in &rels {
        let abs = root.join(r);
        let Ok(text) = std::fs::read_to_string(&abs) else {
            continue;
        };
        for dep in extract_dependencies(&text) {
            if let Some(target) = resolve(r, &dep, &file_set) {
                if target == *r {
                    continue;
                }
                let key = (r.clone(), target.clone());
                if seen_edge.insert(key) {
                    *degree.entry(r.clone()).or_default() += 1;
                    *degree.entry(target.clone()).or_default() += 1;
                    edges.push(TwinEdge {
                        source: r.clone(),
                        target,
                    });
                }
            }
        }
    }

    let nodes = rels
        .iter()
        .map(|r| TwinNode {
            id: r.clone(),
            kind: classify(r).to_string(),
            label: file_name(r).to_string(),
            path: r.clone(),
            degree: *degree.get(r).unwrap_or(&0),
        })
        .collect();

    TwinGraph { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_alias_imports() {
        let files: HashSet<String> = [
            "src/app/page.tsx",
            "src/lib/util.ts",
            "src/components/Nav/index.tsx",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        // relative with extension inference
        assert_eq!(
            resolve("src/app/page.tsx", "../lib/util", &files).as_deref(),
            Some("src/lib/util.ts")
        );
        // @ alias → src/
        assert_eq!(
            resolve("src/app/page.tsx", "@/lib/util", &files).as_deref(),
            Some("src/lib/util.ts")
        );
        // directory index resolution
        assert_eq!(
            resolve("src/app/page.tsx", "../components/Nav", &files).as_deref(),
            Some("src/components/Nav/index.tsx")
        );
        // external specifier → unresolved
        assert_eq!(resolve("src/app/page.tsx", "react", &files), None);
    }

    #[test]
    fn classifies_roles() {
        assert_eq!(classify("src/hooks/useAuth.ts"), "hook");
        assert_eq!(classify("src/components/Button.tsx"), "component");
        assert_eq!(classify("src/server/api/route.ts"), "page");
        assert_eq!(classify("src-tauri/src/store/mod.rs"), "rust");
    }

    #[test]
    fn builds_graph_with_edges() {
        let root = std::env::temp_dir().join(format!("syn-twin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src/lib")).unwrap();
        std::fs::write(root.join("src/lib/util.ts"), "export const x = 1;\n").unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            "import { x } from './lib/util';\nconsole.log(x);\n",
        )
        .unwrap();

        let g = build(&root);
        assert!(g.nodes.iter().any(|n| n.id == "src/app.ts"));
        assert!(g.nodes.iter().any(|n| n.id == "src/lib/util.ts"));
        assert!(g
            .edges
            .iter()
            .any(|e| e.source == "src/app.ts" && e.target == "src/lib/util.ts"));

        std::fs::remove_dir_all(&root).ok();
    }
}
