//! Folder/file tree for the diff sidebar: built from changed paths, with
//! collapsible folders, single-child folder chains compacted (`src/ui/`), and a
//! flattened list of visible rows for rendering and hit-testing.

use super::{ChangeKind, FileChange};

#[derive(Debug)]
pub struct Node {
    /// Display name; compacted folders show `a/b/c`.
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Index into the change list for files; `None` for folders.
    pub file: Option<usize>,
    pub kind: Option<ChangeKind>,
    pub expanded: bool,
}

impl Node {
    pub fn is_folder(&self) -> bool {
        self.file.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleRow {
    pub node: usize,
    pub depth: u16,
}

#[derive(Debug, Default)]
pub struct FileTree {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    pub visible: Vec<VisibleRow>,
}

impl FileTree {
    pub fn build(files: &[FileChange]) -> Self {
        let mut t = FileTree::default();
        for (i, f) in files.iter().enumerate() {
            let mut parent: Option<usize> = None;
            let mut parts = f.path.split('/').peekable();
            while let Some(part) = parts.next() {
                let is_file = parts.peek().is_none();
                let siblings = match parent {
                    Some(p) => &t.nodes[p].children,
                    None => &t.roots,
                };
                let existing = if is_file {
                    None
                } else {
                    siblings
                        .iter()
                        .copied()
                        .find(|&c| t.nodes[c].is_folder() && t.nodes[c].name == part)
                };
                let idx = match existing {
                    Some(idx) => idx,
                    None => {
                        let idx = t.nodes.len();
                        t.nodes.push(Node {
                            name: part.to_string(),
                            parent,
                            children: Vec::new(),
                            file: is_file.then_some(i),
                            kind: is_file.then_some(f.kind),
                            expanded: true,
                        });
                        match parent {
                            Some(p) => t.nodes[p].children.push(idx),
                            None => t.roots.push(idx),
                        }
                        idx
                    }
                };
                parent = Some(idx);
            }
        }
        t.compact();
        t.sort();
        t.rebuild_visible();
        t
    }

    /// Merge folders that contain exactly one subfolder and nothing else.
    fn compact(&mut self) {
        for i in 0..self.nodes.len() {
            loop {
                if !self.nodes[i].is_folder() || self.nodes[i].children.len() != 1 {
                    break;
                }
                let child = self.nodes[i].children[0];
                if !self.nodes[child].is_folder() {
                    break;
                }
                let child_name = std::mem::take(&mut self.nodes[child].name);
                let grandchildren = std::mem::take(&mut self.nodes[child].children);
                self.nodes[i].name.push('/');
                self.nodes[i].name.push_str(&child_name);
                for &g in &grandchildren {
                    self.nodes[g].parent = Some(i);
                }
                self.nodes[i].children = grandchildren;
                // `child` is now orphaned and unreachable.
            }
        }
    }

    /// Folders first, then files, each alphabetically (case-insensitive).
    /// Compacted folders sort by their first component, matching `diff::display_order`.
    fn sort(&mut self) {
        let key = |n: &Node| {
            let first = n.name.split('/').next().unwrap_or("");
            (!n.is_folder(), first.to_lowercase(), first.to_string())
        };
        let mut lists: Vec<Vec<usize>> = Vec::with_capacity(self.nodes.len() + 1);
        lists.push(std::mem::take(&mut self.roots));
        for n in &mut self.nodes {
            lists.push(std::mem::take(&mut n.children));
        }
        for list in &mut lists {
            list.sort_by_cached_key(|&c| key(&self.nodes[c]));
        }
        let mut it = lists.into_iter();
        self.roots = it.next().unwrap_or_default();
        for (n, children) in self.nodes.iter_mut().zip(it) {
            n.children = children;
        }
    }

    pub fn rebuild_visible(&mut self) {
        let mut out = Vec::with_capacity(self.visible.len().max(self.nodes.len()));
        let mut stack: Vec<(usize, u16)> = self.roots.iter().rev().map(|&r| (r, 0)).collect();
        while let Some((idx, depth)) = stack.pop() {
            out.push(VisibleRow { node: idx, depth });
            let n = &self.nodes[idx];
            if n.is_folder() && n.expanded {
                stack.extend(n.children.iter().rev().map(|&c| (c, depth + 1)));
            }
        }
        self.visible = out;
    }

    pub fn toggle(&mut self, node: usize) {
        if self.nodes[node].is_folder() {
            self.nodes[node].expanded = !self.nodes[node].expanded;
            self.rebuild_visible();
        }
    }

    pub fn set_expanded(&mut self, node: usize, expanded: bool) -> bool {
        let n = &mut self.nodes[node];
        if n.is_folder() && n.expanded != expanded {
            n.expanded = expanded;
            self.rebuild_visible();
            true
        } else {
            false
        }
    }

    pub fn set_all_expanded(&mut self, expanded: bool) {
        for n in &mut self.nodes {
            if n.is_folder() {
                n.expanded = expanded;
            }
        }
        self.rebuild_visible();
    }

    /// Visible row index of the next/previous file node from `from` (exclusive).
    pub fn neighbour_file(&self, from: usize, forward: bool) -> Option<usize> {
        if forward {
            (from + 1..self.visible.len()).find(|&i| !self.nodes[self.visible[i].node].is_folder())
        } else {
            (0..from)
                .rev()
                .find(|&i| !self.nodes[self.visible[i].node].is_folder())
        }
    }
}

/// Selection and scroll state over a `FileTree`'s visible rows.
#[derive(Debug, Clone, Copy, Default)]
pub struct TreeCursor {
    pub sel: usize,
    pub scroll: usize,
    /// Rows visible, set by the renderer.
    pub rows: usize,
}

impl TreeCursor {
    pub fn node(&self, tree: &FileTree) -> Option<usize> {
        tree.visible.get(self.sel).map(|v| v.node)
    }

    /// Change index of the selected row if it is a file.
    pub fn file(&self, tree: &FileTree) -> Option<usize> {
        tree.nodes[self.node(tree)?].file
    }

    pub fn clamp(&mut self, tree: &FileTree) {
        self.sel = self.sel.min(tree.visible.len().saturating_sub(1));
        let max_scroll = tree.visible.len().saturating_sub(self.rows.max(1));
        self.scroll = self.scroll.min(max_scroll);
    }

    pub fn ensure_visible(&mut self) {
        let rows = self.rows.max(1);
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + rows {
            self.scroll = self.sel + 1 - rows;
        }
    }

    /// Select a row (clamped); returns the file now under the cursor, if any.
    pub fn select(&mut self, tree: &FileTree, row: usize) -> Option<usize> {
        if tree.visible.is_empty() {
            return None;
        }
        self.sel = row.min(tree.visible.len() - 1);
        self.ensure_visible();
        self.file(tree)
    }

    pub fn move_by(&mut self, tree: &FileTree, delta: isize) -> Option<usize> {
        let n = tree.visible.len() as isize;
        if n == 0 {
            return None;
        }
        let target = (self.sel as isize + delta).clamp(0, n - 1);
        self.select(tree, target as usize)
    }

    /// Jump to the next/previous file row.
    pub fn step_file(&mut self, tree: &FileTree, forward: bool) -> Option<usize> {
        let row = tree.neighbour_file(self.sel, forward)?;
        self.select(tree, row)
    }

    /// Select the first file in display order.
    pub fn first_file(&mut self, tree: &FileTree) -> Option<usize> {
        let row = tree
            .visible
            .iter()
            .position(|v| !tree.nodes[v.node].is_folder())?;
        self.select(tree, row)
    }

    /// Toggle the selected folder; returns `false` if the selection is a file.
    pub fn toggle(&mut self, tree: &mut FileTree) -> bool {
        let Some(node) = self.node(tree) else {
            return false;
        };
        if !tree.nodes[node].is_folder() {
            return false;
        }
        tree.toggle(node);
        self.clamp(tree);
        true
    }

    /// Left: collapse an expanded folder, otherwise jump to the parent.
    pub fn left(&mut self, tree: &mut FileTree) {
        let Some(node) = self.node(tree) else { return };
        if tree.nodes[node].is_folder() && tree.set_expanded(node, false) {
            return;
        }
        if let Some(p) = tree.nodes[node].parent
            && let Some(row) = tree.visible.iter().position(|v| v.node == p)
        {
            self.sel = row;
            self.ensure_visible();
        }
    }

    /// Right: expand a collapsed folder or step into an expanded one.
    /// Returns `true` if the selection is a file (nothing to expand).
    pub fn right(&mut self, tree: &mut FileTree) -> bool {
        let Some(node) = self.node(tree) else {
            return false;
        };
        if !tree.nodes[node].is_folder() {
            return true;
        }
        if !tree.set_expanded(node, true) && !tree.nodes[node].children.is_empty() {
            self.select(tree, self.sel + 1);
        }
        false
    }

    pub fn set_all_expanded(&mut self, tree: &mut FileTree, expanded: bool) {
        let keep = self.node(tree);
        tree.set_all_expanded(expanded);
        self.sel = keep
            .and_then(|n| tree.visible.iter().position(|v| v.node == n))
            .or_else(|| {
                // Selected node got hidden: select its nearest visible ancestor.
                let mut n = keep?;
                while let Some(p) = tree.nodes[n].parent {
                    if let Some(row) = tree.visible.iter().position(|v| v.node == p) {
                        return Some(row);
                    }
                    n = p;
                }
                None
            })
            .unwrap_or(0);
        self.ensure_visible();
    }

    pub fn scroll_view(&mut self, tree: &FileTree, delta: isize) {
        let max = tree.visible.len().saturating_sub(self.rows.max(1)) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, kind: ChangeKind) -> FileChange {
        FileChange {
            worktree: false,
            remote: None,
            path: path.to_string(),
            old_path: None,
            kind,
            old: None,
            new: None,
        }
    }

    fn names(t: &FileTree) -> Vec<String> {
        t.visible
            .iter()
            .map(|v| format!("{}{}", " ".repeat(v.depth as usize), t.nodes[v.node].name))
            .collect()
    }

    #[test]
    fn builds_compacts_and_sorts() {
        let files = vec![
            change("src/ui/list.rs", ChangeKind::Modified),
            change("README.md", ChangeKind::Added),
            change("src/ui/mod.rs", ChangeKind::Modified),
            change("Cargo.toml", ChangeKind::Modified),
            change("docs/a/b/c.txt", ChangeKind::Deleted),
        ];
        let t = FileTree::build(&files);
        assert_eq!(
            names(&t),
            vec![
                "docs/a/b",
                " c.txt",
                "src/ui",
                " list.rs",
                " mod.rs",
                "Cargo.toml",
                "README.md"
            ]
        );
    }

    #[test]
    fn collapse_and_reveal() {
        let files = vec![
            change("a/x.rs", ChangeKind::Modified),
            change("a/y.rs", ChangeKind::Modified),
            change("b.rs", ChangeKind::Modified),
        ];
        let mut t = FileTree::build(&files);
        let a = t.visible[0].node;
        t.toggle(a);
        assert_eq!(names(&t), vec!["a", "b.rs"]);
        assert!(t.set_expanded(a, true));
        assert!(!t.set_expanded(a, true), "already expanded");
        assert_eq!(names(&t), vec!["a", " x.rs", " y.rs", "b.rs"]);
        assert_eq!(t.neighbour_file(2, true), Some(3));
        assert_eq!(t.neighbour_file(1, false), None);
        t.set_all_expanded(false);
        assert_eq!(t.visible.len(), 2);
    }
}
