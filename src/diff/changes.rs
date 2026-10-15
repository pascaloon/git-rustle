//! The set of files changed between two commits, with the sidebar tree and
//! per-file line counts that stream in from the stats worker. Shared by the
//! details panel (commit vs. parent) and the full-screen diff view.

use std::sync::Arc;

use gix::ObjectId;

use super::filetree::FileTree;
use super::{FileChange, Stat, TreeDiff};

#[derive(Debug)]
pub struct ChangeSet {
    /// Identifies this set in worker messages.
    pub key: u32,
    /// `None` means the empty tree (root commit).
    pub old: Option<ObjectId>,
    pub new: ObjectId,
    pub files: Arc<Vec<FileChange>>,
    pub tree: FileTree,
    /// Tree diff requested from the worker.
    pub requested: bool,
    /// Tree diff arrived (possibly with no files).
    pub loaded: bool,
    pub error: Option<String>,
    /// Per-file stats; `None` until computed.
    pub stats: Vec<Option<Stat>>,
    pub stats_pending: usize,
    /// Per-node totals (folders sum their descendants) and whether any
    /// descendant has counts yet.
    totals: Vec<(u32, u32, bool)>,
}

impl ChangeSet {
    pub fn new(key: u32, old: Option<ObjectId>, new: ObjectId) -> Self {
        ChangeSet {
            key,
            old,
            new,
            files: Arc::new(Vec::new()),
            tree: FileTree::default(),
            requested: false,
            loaded: false,
            error: None,
            stats: Vec::new(),
            stats_pending: 0,
            totals: Vec::new(),
        }
    }

    pub fn set_tree(&mut self, result: Result<TreeDiff, String>) {
        match result {
            Ok(td) => {
                self.tree = FileTree::build(&td.files);
                self.stats = vec![None; td.files.len()];
                self.stats_pending = td.files.len();
                self.totals = vec![(0, 0, false); self.tree.nodes.len()];
                self.files = Arc::new(td.files);
            }
            Err(e) => self.error = Some(e),
        }
        self.loaded = true;
    }

    /// Stats are only needed when there are files without them.
    pub fn wants_stats(&self) -> bool {
        self.loaded && self.stats_pending > 0
    }

    pub fn apply_stats(&mut self, chunk: &[(usize, Stat)]) {
        for &(file, stat) in chunk {
            if let Some(slot) = self.stats.get_mut(file)
                && slot.is_none()
            {
                *slot = Some(stat);
                self.stats_pending -= 1;
            }
        }
        self.recompute_totals();
    }

    fn recompute_totals(&mut self) {
        self.totals.iter_mut().for_each(|t| *t = (0, 0, false));
        // Files first, then propagate to ancestors.
        for (i, n) in self.tree.nodes.iter().enumerate() {
            let Some(file) = n.file else { continue };
            let Some(Some(Stat::Lines { added, removed })) = self.stats.get(file) else { continue };
            let mut cur = Some(i);
            while let Some(c) = cur {
                let t = &mut self.totals[c];
                t.0 += added;
                t.1 += removed;
                t.2 = true;
                cur = self.tree.nodes[c].parent;
            }
        }
    }

    /// `Some((added, removed))` for a node, `None` when unknown (pending or binary).
    pub fn node_total(&self, node: usize) -> Option<(u32, u32)> {
        let n = &self.tree.nodes[node];
        match n.file {
            Some(file) => match self.stats.get(file).copied().flatten() {
                Some(Stat::Lines { added, removed }) => Some((added, removed)),
                _ => None,
            },
            None => self
                .totals
                .get(node)
                .filter(|t| t.2)
                .map(|t| (t.0, t.1)),
        }
    }

    /// Whole-set totals.
    pub fn total(&self) -> (u32, u32) {
        self.stats
            .iter()
            .flatten()
            .fold((0, 0), |(a, r), s| match s {
                Stat::Lines { added, removed } => (a + added, r + removed),
                Stat::Opaque => (a, r),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::ChangeKind;

    #[test]
    fn totals_roll_up_to_folders() {
        let change = |p: &str| FileChange {
            path: p.to_string(),
            old_path: None,
            kind: ChangeKind::Modified,
            old: None,
            new: None,
        };
        let mut cs = ChangeSet::new(1, None, ObjectId::null(gix::hash::Kind::Sha1));
        cs.set_tree(Ok(TreeDiff {
            files: vec![change("src/a.rs"), change("src/b.rs"), change("img.png")],
        }));
        assert!(cs.wants_stats());
        cs.apply_stats(&[(0, Stat::Lines { added: 5, removed: 1 }), (2, Stat::Opaque)]);
        assert_eq!(cs.stats_pending, 1);
        let src = cs.tree.visible[0].node;
        assert_eq!(cs.node_total(src), Some((5, 1)));
        cs.apply_stats(&[(1, Stat::Lines { added: 2, removed: 2 })]);
        assert_eq!(cs.node_total(src), Some((7, 3)));
        assert_eq!(cs.total(), (7, 3));
        let img = cs.tree.visible[3].node;
        assert_eq!(cs.node_total(img), None, "binary has no counts");
        assert!(!cs.wants_stats());
        // Duplicate delivery is ignored.
        cs.apply_stats(&[(1, Stat::Lines { added: 9, removed: 9 })]);
        assert_eq!(cs.total(), (7, 3));
    }
}
