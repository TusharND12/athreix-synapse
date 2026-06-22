//! Time Machine — checkpoints & restore, built on the content-addressed snapshot
//! store. A checkpoint captures the whole working tree as a `path -> blob hash`
//! map; restoring writes those blobs back. Entirely independent of the user's
//! own git history. See docs/02-event-model.md and docs/03-database-schema.md.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::snapshots::Snapshots;
use crate::store::{CheckpointInfo, Db};
use crate::watcher::is_ignored;

/// Cap on a single file we'll snapshot into a checkpoint (skip huge binaries).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Recursively collect non-ignored files under `root`.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_ignored(&path, root) {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => collect_files(root, &path, out),
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

/// Snapshot the current working tree and record a checkpoint.
pub fn create(
    root: &Path,
    db: &Db,
    snaps: &Snapshots,
    id: &str,
    ts: i64,
    label: &str,
    auto: bool,
) -> Result<CheckpointInfo, String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files);

    let mut tree: BTreeMap<String, String> = BTreeMap::new();
    for path in &files {
        let too_big = std::fs::metadata(path)
            .map(|m| m.len() > MAX_FILE_BYTES)
            .unwrap_or(true);
        if too_big {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(hash) = snaps.write_blob(&bytes) {
                tree.insert(rel(root, path), hash);
            }
        }
    }

    let tree_json = serde_json::to_string(&tree).map_err(|e| e.to_string())?;
    db.insert_checkpoint(id, ts, label, &tree_json, auto)
        .map_err(|e| e.to_string())?;

    Ok(CheckpointInfo {
        id: id.to_string(),
        ts,
        label: label.to_string(),
        file_count: tree.len() as i64,
        auto,
    })
}

/// Restore the working tree to a checkpoint: write back every blob in the tree,
/// and delete tracked files that did not exist at that point. Returns
/// (files_written, files_deleted).
pub fn restore(
    root: &Path,
    db: &Db,
    snaps: &Snapshots,
    id: &str,
) -> Result<(usize, usize), String> {
    let tree_json = db
        .get_checkpoint_tree(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("checkpoint not found: {id}"))?;
    let tree: BTreeMap<String, String> =
        serde_json::from_str(&tree_json).map_err(|e| e.to_string())?;
    Ok(restore_tree(root, snaps, &tree))
}

/// Write every blob in `tree` back to the working tree. Returns files written.
fn write_tree(root: &Path, snaps: &Snapshots, tree: &BTreeMap<String, String>) -> usize {
    let mut written = 0usize;
    for (rel_path, hash) in tree {
        let abs = root.join(rel_path);
        if let Ok(bytes) = snaps.read_blob(hash) {
            if let Some(parent) = abs.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&abs, bytes).is_ok() {
                written += 1;
            }
        }
    }
    written
}

/// Restore a full checkpoint tree: write its blobs back and delete every other
/// working-tree file (a checkpoint is a complete snapshot, so this is faithful).
pub fn restore_tree(root: &Path, snaps: &Snapshots, tree: &BTreeMap<String, String>) -> (usize, usize) {
    let written = write_tree(root, snaps, tree);
    let mut current = Vec::new();
    collect_files(root, root, &mut current);
    let mut deleted = 0usize;
    for path in current {
        let r = rel(root, &path);
        if !tree.contains_key(&r) && std::fs::remove_file(&path).is_ok() {
            deleted += 1;
        }
    }
    (written, deleted)
}

/// Restore a *partial* (event-history) tree safely: write its blobs back, but
/// only delete files Synapse actually has history for (`tracked`). Files that
/// existed before Synapse started watching — and were never touched — are left
/// untouched, so a time-rewind can never destroy unhistoried data.
pub fn restore_tree_scoped(
    root: &Path,
    snaps: &Snapshots,
    tree: &BTreeMap<String, String>,
    tracked: &HashSet<String>,
) -> (usize, usize) {
    let written = write_tree(root, snaps, tree);
    let mut current = Vec::new();
    collect_files(root, root, &mut current);
    let mut deleted = 0usize;
    for path in current {
        let r = rel(root, &path);
        if tracked.contains(&r) && !tree.contains_key(&r) && std::fs::remove_file(&path).is_ok() {
            deleted += 1;
        }
    }
    (written, deleted)
}

/// Fold the file-version history into the project's snapshot tree at time `at`
/// (the rewind / scrubber engine). `versions` must be (ts, path, kind, blob)
/// sorted ascending by ts.
pub fn state_at(
    versions: &[(i64, String, String, Option<String>)],
    at: i64,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (ts, path, kind, blob) in versions {
        if *ts > at {
            break;
        }
        if kind == "deleted" {
            map.remove(path);
        } else if let Some(h) = blob {
            map.insert(path.clone(), h.clone());
        }
    }
    map
}

/// Restore the working tree to its state at timestamp `at`. Safe: only files
/// Synapse has history for can be removed; pre-existing untouched files are kept.
pub fn restore_at(root: &Path, db: &Db, snaps: &Snapshots, at: i64) -> Result<(usize, usize), String> {
    let versions = db.file_versions().map_err(|e| e.to_string())?;
    let tree = state_at(&versions, at);
    let tracked: HashSet<String> = versions.into_iter().map(|(_, p, _, _)| p).collect();
    Ok(restore_tree_scoped(root, snaps, &tree, &tracked))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("syn-tm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn checkpoint_then_restore_roundtrips_tree() {
        let root = temp();
        let synapse_dir = root.join(".synapse");
        std::fs::create_dir_all(&synapse_dir).unwrap();
        let db = Db::open(&synapse_dir.join("t.db")).unwrap();
        let snaps = Snapshots::open(&synapse_dir).unwrap();

        // Initial state: two files.
        std::fs::write(root.join("a.txt"), b"hello\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}\n").unwrap();

        let cp = create(&root, &db, &snaps, "cp1", 1, "initial", false).unwrap();
        assert_eq!(cp.file_count, 2);

        // Mutate: change a.txt, add c.txt, delete src/main.rs.
        std::fs::write(root.join("a.txt"), b"changed\n").unwrap();
        std::fs::write(root.join("c.txt"), b"new file\n").unwrap();
        std::fs::remove_file(root.join("src/main.rs")).unwrap();

        let (written, deleted) = restore(&root, &db, &snaps, "cp1").unwrap();
        assert_eq!(written, 2, "both checkpointed files rewritten");
        assert_eq!(deleted, 1, "c.txt (created after) removed");

        // Working tree now matches the checkpoint exactly.
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"hello\n");
        assert_eq!(std::fs::read(root.join("src/main.rs")).unwrap(), b"fn main() {}\n");
        assert!(!root.join("c.txt").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scoped_restore_never_deletes_untracked_files() {
        let root = temp();
        let synapse_dir = root.join(".synapse");
        std::fs::create_dir_all(&synapse_dir).unwrap();
        let snaps = Snapshots::open(&synapse_dir).unwrap();

        // a.txt = tracked & present at target; gone.txt = tracked but created after
        // target; keep.txt = pre-existing, never tracked.
        let blob_a = snaps.write_blob(b"A-at-target\n").unwrap();
        std::fs::write(root.join("a.txt"), b"A-changed\n").unwrap();
        std::fs::write(root.join("gone.txt"), b"made later\n").unwrap();
        std::fs::write(root.join("keep.txt"), b"pre-existing\n").unwrap();

        let mut tree = BTreeMap::new();
        tree.insert("a.txt".to_string(), blob_a);
        let tracked: HashSet<String> =
            ["a.txt", "gone.txt"].iter().map(|s| s.to_string()).collect();

        let (written, deleted) = restore_tree_scoped(&root, &snaps, &tree, &tracked);
        assert_eq!(written, 1);
        assert_eq!(deleted, 1, "only the tracked-but-absent file is removed");
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"A-at-target\n");
        assert!(!root.join("gone.txt").exists(), "tracked file created later is removed");
        assert!(root.join("keep.txt").exists(), "untracked pre-existing file is KEPT");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn state_at_folds_history() {
        // a.txt created@10 (h1), modified@20 (h2); b.txt created@15 (hb), deleted@25.
        let versions = vec![
            (10, "a.txt".into(), "created".into(), Some("h1".into())),
            (15, "b.txt".into(), "created".into(), Some("hb".into())),
            (20, "a.txt".into(), "modified".into(), Some("h2".into())),
            (25, "b.txt".into(), "deleted".into(), None),
        ];
        // At t=17: a.txt=h1, b.txt=hb.
        let s17 = state_at(&versions, 17);
        assert_eq!(s17.get("a.txt").map(|s| s.as_str()), Some("h1"));
        assert_eq!(s17.get("b.txt").map(|s| s.as_str()), Some("hb"));
        // At t=30: a.txt=h2, b.txt gone.
        let s30 = state_at(&versions, 30);
        assert_eq!(s30.get("a.txt").map(|s| s.as_str()), Some("h2"));
        assert!(!s30.contains_key("b.txt"));
    }
}
