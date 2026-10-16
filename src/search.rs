//! Lazy, cancellable graph search. Immutable messages and changed paths are
//! indexed on a worker; typing never performs object reads or tree comparisons.
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::commit::ref_iter::Token;
use gix::prelude::FindExt;

use crate::graph::layout::{GraphCell, Layout};
use crate::model::History;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    All,
    Author,
    Message,
    Sha,
    File,
}

pub struct Query {
    pub field: Field,
    terms: Vec<String>,
}
impl Query {
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        let (field, value) = match text.get(..2) {
            Some("a:") => (Field::Author, &text[2..]),
            Some("m:") => (Field::Message, &text[2..]),
            Some("s:") => (Field::Sha, &text[2..]),
            Some("f:") => (Field::File, &text[2..]),
            _ => (Field::All, text),
        };
        Self {
            field,
            terms: value.split_whitespace().map(str::to_lowercase).collect(),
        }
    }
    /// Each term is a contiguous substring, including inside a longer word.
    /// All terms must match the same field; their order is unimportant.
    pub fn matches(&self, normalized: &str) -> bool {
        self.terms
            .iter()
            .all(|term| normalized.contains(term.as_str()))
    }
}

#[derive(Default)]
pub struct Search {
    pub text: crate::pending::Text,
    pub editing: bool,
    pub generation: u64,
    pub wanted: bool,
    pub busy: bool,
    pub result: Option<Matches>,
    pub input: Option<Arc<Input>>,
}
impl Search {
    pub fn active(&self) -> bool {
        !self.text.value.trim().is_empty()
    }
    pub fn changed(&mut self) {
        self.generation += 1;
        self.wanted = self.active();
        self.busy = self.wanted;
        self.result = None;
    }
    pub fn invalidate(&mut self) {
        self.input = None;
        if self.active() {
            self.changed();
        }
    }
}

pub struct Input {
    pub history: History,
    pub labels: HashMap<ObjectId, Vec<String>>,
    pub ghosts: HashMap<ObjectId, crate::git::CommitDetails>,
    pub sources: HashMap<ObjectId, crate::remote::Remote>,
}
pub struct Request {
    pub generation: u64,
    pub text: String,
    pub input: Arc<Input>,
}
#[derive(Clone)]
pub struct Matches {
    pub generation: u64,
    pub rows: Vec<usize>,
    pub positions: Vec<usize>,
    pub graph: Vec<SearchRow>,
    pub examined: usize,
    pub total: usize,
    pub done: bool,
    pub unavailable: usize,
    pub error: Option<String>,
}
pub type SearchRow = (Vec<GraphCell>, u16, Vec<u8>);

/// Contract hidden ancestry to the nearest visible ancestors, preserving merges.
/// Explicit DFS avoids overflowing the stack on long linear histories.
fn union(a: &Arc<Vec<usize>>, b: &Arc<Vec<usize>>) -> Arc<Vec<usize>> {
    if Arc::ptr_eq(a, b) || b.is_empty() {
        return a.clone();
    }
    if a.is_empty() {
        return b.clone();
    }
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    if out.len() == a.len() {
        a.clone()
    } else if out.len() == b.len() {
        b.clone()
    } else {
        Arc::new(out)
    }
}

fn graph(
    history: &History,
    parents: &[Vec<usize>],
    rows: &[usize],
    ghosts: &HashMap<ObjectId, crate::git::CommitDetails>,
    cancelled: &dyn Fn() -> bool,
) -> Vec<SearchRow> {
    if rows.is_empty() {
        return Vec::new();
    }
    if rows.len() == history.len() {
        return rows
            .iter()
            .map(|&i| {
                (
                    history.row_cells(i).to_vec(),
                    history.commits[i].node_lane,
                    history
                        .remote_cells
                        .get(history.commits[i].row.range())
                        .unwrap_or(&[])
                        .to_vec(),
                )
            })
            .collect();
    }
    let mut visible = vec![false; history.len()];
    for &i in rows {
        visible[i] = true;
    }
    let mut state = vec![0u8; history.len()];
    let empty = Arc::new(Vec::new());
    let mut nearest = vec![empty.clone(); history.len()];
    for &row in rows {
        for &root in &parents[row] {
            let mut stack = vec![(root, false)];
            while let Some((i, exit)) = stack.pop() {
                if cancelled() {
                    return Vec::new();
                }
                if state[i] == 2 {
                    continue;
                }
                if visible[i] {
                    nearest[i] = Arc::new(vec![i]);
                    state[i] = 2;
                    continue;
                }
                if exit {
                    let mut found = empty.clone();
                    for &p in &parents[i] {
                        found = union(&found, &nearest[p]);
                    }
                    nearest[i] = found;
                    state[i] = 2;
                } else if state[i] == 0 {
                    state[i] = 1;
                    stack.push((i, true));
                    for &p in &parents[i] {
                        stack.push((p, false));
                    }
                }
            }
        }
    }
    let mut layout = Layout::new();
    let remote = rows
        .iter()
        .any(|&i| ghosts.contains_key(&history.commits[i].id));
    let mut filtered = History::default();
    let mut graph: Vec<SearchRow> = rows
        .iter()
        .map(|&i| {
            if cancelled() {
                return (Vec::new(), 0, Vec::new());
            }
            let mut found = empty.clone();
            for &p in &parents[i] {
                found = union(&found, &nearest[p]);
            }
            let projected: Vec<_> = found
                .iter()
                .filter(|&&ancestor| ancestor > i)
                .map(|&ancestor| history.commits[ancestor].id)
                .collect();
            let row = layout.push(&history.commits[i].id, &projected);
            if remote {
                let mut rec = history.commits[i];
                rec.row = filtered.push_cells(&row.cells);
                rec.parents = filtered.push_parents(&projected);
                rec.node_lane = row.node_lane;
                filtered.commits.push(rec);
            }
            (row.cells, row.node_lane, Vec::new())
        })
        .collect();
    if remote && !cancelled() {
        let flags = crate::graph::dimming::build(&filtered, |id| ghosts.contains_key(id));
        for (row, rec) in graph.iter_mut().zip(&filtered.commits) {
            row.2 = flags.get(rec.row.range()).unwrap_or(&[]).to_vec();
        }
    }
    graph
}

#[derive(Default)]
struct Document {
    message: Option<String>,
    paths: Option<Vec<u32>>,
}
struct Base {
    input: Arc<Input>,
    sha: Vec<String>,
    authors: Vec<String>,
    labels: Vec<Vec<String>>,
    parents: Vec<Vec<usize>>,
    order: Arc<Vec<usize>>,
    file_order: Arc<Vec<usize>>,
    local_files: usize,
}
impl Base {
    fn new(input: Arc<Input>) -> Self {
        let history = &input.history;
        let by_id: HashMap<_, _> = history
            .commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();
        let order = Arc::new((0..history.len()).collect::<Vec<_>>());
        let local_files = history
            .commits
            .iter()
            .filter(|c| !input.sources.contains_key(&c.id))
            .count();
        let file_order = if local_files == history.len() {
            order.clone()
        } else {
            Arc::new(
                order
                    .iter()
                    .copied()
                    .filter(|&i| !input.sources.contains_key(&history.commits[i].id))
                    .chain(
                        order
                            .iter()
                            .copied()
                            .filter(|&i| input.sources.contains_key(&history.commits[i].id)),
                    )
                    .collect(),
            )
        };
        Self {
            order,
            file_order,
            local_files,
            sha: history.commits.iter().map(|c| c.id.to_string()).collect(),
            authors: (0..history.len())
                .map(|i| history.author(i).to_lowercase())
                .collect(),
            labels: history
                .commits
                .iter()
                .map(|c| {
                    input
                        .labels
                        .get(&c.id)
                        .map_or_else(Vec::new, |ls| ls.iter().map(|s| s.to_lowercase()).collect())
                })
                .collect(),
            parents: (0..history.len())
                .map(|i| {
                    history
                        .parents(i)
                        .iter()
                        .filter_map(|p| by_id.get(p).copied())
                        .collect()
                })
                .collect(),
            input,
        }
    }
}
#[derive(Default)]
pub struct Index {
    base: Option<Base>,
    docs: HashMap<ObjectId, Document>,
    paths: Vec<String>,
    path_ids: HashMap<String, u32>,
    loaded_messages: bool,
    loaded_paths: bool,
    message_reader: Option<BufReader<std::fs::File>>,
    path_reader: Option<BufReader<std::fs::File>>,
}
impl Index {
    fn intern(&mut self, path: String) -> u32 {
        let path = path.to_lowercase();
        if let Some(&id) = self.path_ids.get(&path) {
            return id;
        }
        let id = self.paths.len() as u32;
        self.path_ids.insert(path.clone(), id);
        self.paths.push(path);
        id
    }
    fn cache_path(repo: &gix::Repository, files: bool) -> Option<PathBuf> {
        crate::cache::cache_path(&crate::git::git_dir(repo)).map(|p| {
            p.with_extension(if files {
                "search-paths-v1"
            } else {
                "search-messages-v1"
            })
        })
    }
    fn load(&mut self, repo: &gix::Repository, files: bool, latest: &AtomicU64, generation: u64) {
        let loaded = if files {
            self.loaded_paths
        } else {
            self.loaded_messages
        };
        if loaded {
            return;
        }
        let paused = if files {
            self.path_reader.take()
        } else {
            self.message_reader.take()
        };
        let reader = paused.or_else(|| {
            Self::cache_path(repo, files)
                .and_then(|p| std::fs::File::open(p).ok())
                .map(BufReader::new)
        });
        if let Some(mut reader) = reader {
            let mut line = String::new();
            loop {
                if latest.load(Ordering::Relaxed) != generation {
                    if files {
                        self.path_reader = Some(reader);
                    } else {
                        self.message_reader = Some(reader);
                    }
                    return;
                }
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if files {
                    if let Ok((id, paths)) = serde_json::from_str::<(String, Vec<String>)>(&line)
                        && let Ok(id) = ObjectId::from_hex(id.as_bytes())
                    {
                        let paths = paths.into_iter().map(|p| self.intern(p)).collect();
                        self.docs.entry(id).or_default().paths = Some(paths);
                    }
                } else if let Ok((id, message)) = serde_json::from_str::<(String, String)>(&line)
                    && let Ok(id) = ObjectId::from_hex(id.as_bytes())
                {
                    self.docs.entry(id).or_default().message = Some(message);
                }
            }
        }
        if files {
            self.loaded_paths = true;
        } else {
            self.loaded_messages = true;
        }
    }
    pub fn run(
        &mut self,
        repo: &gix::Repository,
        req: Request,
        latest: &AtomicU64,
        no_cache: bool,
        mut send: impl FnMut(Matches),
    ) {
        if self
            .base
            .as_ref()
            .is_none_or(|b| !Arc::ptr_eq(&b.input, &req.input))
        {
            self.base = Some(Base::new(req.input.clone()));
        }
        let query = Query::parse(&req.text);
        if latest.load(Ordering::Relaxed) != req.generation {
            return;
        }
        if query.terms.is_empty() {
            let base = self.base.as_ref().unwrap();
            let n = base.input.history.len();
            let mut result = Self::result(req.generation, base, (0..n).collect(), n, true, 0, None);
            result.graph = graph(
                &base.input.history,
                &base.parents,
                &result.rows,
                &base.input.ghosts,
                &|| latest.load(Ordering::Relaxed) != req.generation,
            );
            if latest.load(Ordering::Relaxed) == req.generation {
                send(result);
            }
            return;
        }
        let files = query.field == Field::File;
        let messages = matches!(query.field, Field::All | Field::Message);
        if !no_cache && (files || messages) {
            self.load(repo, files, latest, req.generation);
        }
        if latest.load(Ordering::Relaxed) != req.generation {
            return;
        }
        let mut writer = if !no_cache && (files || messages) {
            Self::cache_path(repo, files).and_then(|p| {
                std::fs::create_dir_all(p.parent()?).ok()?;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .ok()
                    .map(BufWriter::new)
            })
        } else {
            None
        };
        let history = &req.input.history;
        // A path is matched once per query, shared by every commit touching it.
        let mut path_matches: Vec<bool> = self.paths.iter().map(|p| query.matches(p)).collect();
        let mut rows = Vec::new();
        let mut unavailable = 0;
        let mut error = None;
        let mut last_send = Instant::now();
        let mut buf = Vec::new();
        let base = self.base.as_ref().unwrap();
        let order = if files {
            base.file_order.clone()
        } else {
            base.order.clone()
        };
        let local_files = base.local_files;
        for (examined, &i) in order.iter().enumerate() {
            if latest.load(Ordering::Relaxed) != req.generation {
                return;
            }
            // Cached/local file matches stay usable while ghost API requests run.
            if files && examined == local_files && examined > 0 {
                send(Self::result(
                    req.generation,
                    self.base.as_ref().unwrap(),
                    rows.clone(),
                    examined,
                    false,
                    unavailable,
                    error.clone(),
                ));
                if latest.load(Ordering::Relaxed) != req.generation {
                    return;
                }
            }
            let commit = &history.commits[i];
            let ghost = req.input.ghosts.get(&commit.id);
            let mut failed = false;
            if messages
                && self
                    .docs
                    .get(&commit.id)
                    .and_then(|d| d.message.as_ref())
                    .is_none()
            {
                let message = if let Some(g) = ghost {
                    Ok(g.message.clone())
                } else {
                    repo.objects
                        .find_commit_iter(&commit.id, &mut buf)
                        .map_err(|e| e.to_string())
                        .and_then(|iter| {
                            for token in iter {
                                if let Token::Message(message) = token.map_err(|e| e.to_string())? {
                                    return Ok(message.to_str_lossy().into_owned());
                                }
                            }
                            Err("Commit message unavailable".into())
                        })
                };
                match message {
                    Ok(message) => {
                        let message = message.to_lowercase();
                        if ghost.is_none()
                            && let Some(w) = &mut writer
                        {
                            let _ =
                                serde_json::to_writer(&mut *w, &(commit.id.to_string(), &message));
                            let _ = w.write_all(b"\n");
                        }
                        self.docs.entry(commit.id).or_default().message = Some(message);
                    }
                    Err(e) => {
                        failed = true;
                        error.get_or_insert(e);
                    }
                }
            }
            if files
                && self
                    .docs
                    .get(&commit.id)
                    .and_then(|d| d.paths.as_ref())
                    .is_none()
            {
                let old = history.parents(i).first();
                let paths = if let Some(remote) = req.input.sources.get(&commit.id) {
                    crate::remote::files::tree(repo, remote, old.copied(), commit.id, no_cache).map(
                        |diff| {
                            let mut paths = HashSet::new();
                            for file in diff.files {
                                paths.insert(file.path);
                                if let Some(old) = file.old_path {
                                    paths.insert(old);
                                }
                            }
                            paths.into_iter().collect::<Vec<_>>()
                        },
                    )
                } else {
                    crate::diff::changed_paths(repo, old, &commit.id).map_err(|e| e.to_string())
                };
                match paths {
                    Ok(paths) => {
                        if ghost.is_none()
                            && let Some(w) = &mut writer
                        {
                            let _ =
                                serde_json::to_writer(&mut *w, &(commit.id.to_string(), &paths));
                            let _ = w.write_all(b"\n");
                        }
                        let ids = paths.into_iter().map(|p| self.intern(p)).collect();
                        self.docs.entry(commit.id).or_default().paths = Some(ids);
                        while path_matches.len() < self.paths.len() {
                            path_matches.push(query.matches(&self.paths[path_matches.len()]));
                        }
                    }
                    Err(e) => {
                        failed = true;
                        error.get_or_insert(e);
                    }
                }
            }
            if failed {
                unavailable += 1;
            }
            let base = self.base.as_ref().unwrap();
            let sha = || query.matches(&base.sha[i]);
            let author = || query.matches(&base.authors[i]);
            let message = || {
                self.docs
                    .get(&commit.id)
                    .and_then(|d| d.message.as_ref())
                    .is_some_and(|s| query.matches(s))
            };
            let matched = match query.field {
                Field::Sha => sha(),
                Field::Author => author(),
                Field::Message => message(),
                Field::File => self
                    .docs
                    .get(&commit.id)
                    .and_then(|d| d.paths.as_ref())
                    .is_some_and(|paths| paths.iter().any(|&p| path_matches[p as usize])),
                Field::All => {
                    sha()
                        || author()
                        || message()
                        || base.labels[i].iter().any(|s| query.matches(s))
                }
            };
            if matched {
                rows.push(i);
            }
            if last_send.elapsed() >= Duration::from_millis(200) && examined + 1 < history.len() {
                send(Self::result(
                    req.generation,
                    self.base.as_ref().unwrap(),
                    rows.clone(),
                    examined + 1,
                    false,
                    unavailable,
                    error.clone(),
                ));
                last_send = Instant::now();
            }
        }
        let mut result = Self::result(
            req.generation,
            self.base.as_ref().unwrap(),
            rows,
            history.len(),
            false,
            unavailable,
            error,
        );
        // Publish fully matched rows before routing edges. Users can navigate,
        // mark and diff immediately, even for sparse results across many merges.
        send(result.clone());
        result.graph = graph(
            history,
            &self.base.as_ref().unwrap().parents,
            &result.rows,
            &req.input.ghosts,
            &|| latest.load(Ordering::Relaxed) != req.generation,
        );
        if latest.load(Ordering::Relaxed) != req.generation {
            return;
        }
        if let Some(writer) = &mut writer {
            let _ = writer.flush();
        }
        result.done = true;
        send(result);
    }
    fn result(
        generation: u64,
        base: &Base,
        mut rows: Vec<usize>,
        examined: usize,
        done: bool,
        unavailable: usize,
        error: Option<String>,
    ) -> Matches {
        let history = &base.input.history;
        rows.sort_unstable();
        let mut positions = vec![usize::MAX; history.len()];
        for (row, &index) in rows.iter().enumerate() {
            positions[index] = row;
        }
        Matches {
            generation,
            rows,
            positions,
            graph: Vec::new(),
            examined,
            total: history.len(),
            done,
            unavailable,
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Focus, Mode};
    use crate::term::{KeyCode, KeyEvent};
    use std::path::Path;

    fn git(path: &Path, args: &[&str]) -> String {
        if args.first() == Some(&"commit") {
            let date = if args.contains(&"Initial build") {
                "1700000000 +0000"
            } else if args.contains(&"Move build configuration") {
                "1700000010 +0000"
            } else {
                "1700000020 +0000"
            };
            let output = crate::git::command::run_env(
                path,
                args,
                None,
                Duration::from_secs(15),
                &[
                    ("GIT_COMMITTER_DATE", std::ffi::OsStr::new(date)),
                    ("GIT_AUTHOR_DATE", std::ffi::OsStr::new(date)),
                ],
            )
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return String::from_utf8(output.stdout).unwrap().trim().to_string();
        }
        String::from_utf8(crate::git::command::checked(path, args).unwrap())
            .unwrap()
            .trim()
            .to_string()
    }
    fn fixture() -> (tempfile::TempDir, gix::Repository, Arc<Input>) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Bób Builder"),
            ("user.email", "bob@example.org"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "no-hooks"),
        ] {
            git(dir.path(), &["config", key, value]);
        }
        std::fs::write(dir.path().join("build.sharpmake.cs"), "one\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(
            dir.path(),
            &[
                "commit",
                "-m",
                "Initial build",
                "-m",
                "Hidden body zebracorn",
            ],
        );
        git(dir.path(), &["tag", "release-needle"]);
        git(dir.path(), &["mv", "build.sharpmake.cs", "renamed.cs"]);
        git(dir.path(), &["commit", "-m", "Move build configuration"]);
        std::fs::write(dir.path().join("unrelated.txt"), "two\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(
            dir.path(),
            &[
                "commit",
                "--author=Alice <alice@example.org>",
                "-m",
                "Bump thingy",
            ],
        );
        git(dir.path(), &["branch", "feature-needle"]);
        let repo = crate::git::open(dir.path()).unwrap();
        let refs = crate::git::snapshot_refs(&repo).unwrap();
        let mut history = History::default();
        crate::git::walk(&repo, &refs.tips, &mut |h| {
            history.append(h);
            true
        })
        .unwrap();
        let input = Arc::new(Input {
            history,
            labels: refs.refs.into_iter().fold(
                HashMap::<ObjectId, Vec<String>>::new(),
                |mut m, r| {
                    m.entry(r.id).or_default().push(r.name);
                    m
                },
            ),
            ghosts: HashMap::new(),
            sources: HashMap::new(),
        });
        (dir, repo, input)
    }
    fn run(index: &mut Index, repo: &gix::Repository, input: &Arc<Input>, query: &str) -> Matches {
        let latest = AtomicU64::new(1);
        let mut result = None;
        index.run(
            repo,
            Request {
                generation: 1,
                text: query.into(),
                input: input.clone(),
            },
            &latest,
            true,
            |r| result = Some(r),
        );
        result.unwrap()
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    #[test]
    fn terms_are_case_insensitive_unicode_and_prefixes_are_anchored() {
        let q = Query::parse(" a: BÓB ");
        assert_eq!(q.field, Field::Author);
        assert!(q.matches("bób builder"));
        assert!(!q.matches("builder"));
        assert!(Query::parse("m: Bump thingy").matches("bump thingy"));
        assert!(!Query::parse("m: bmp tgy").matches("bump thingy"));
        assert_eq!(Query::parse("fix a: bob").field, Field::All);
        assert_eq!(Query::parse("f:").field, Field::File);
        assert!(Query::parse("m:").matches("anything"));
        assert!(!Query::parse("abc").matches("cba"));
    }

    #[test]
    fn all_query_terms_match_contiguously_including_inside_words() {
        for text in ["mold contracts", "m: mold contracts", "a: mold contracts"] {
            let query = Query::parse(text);
            assert!(query.matches("update mold and contracts"));
            assert!(query.matches("contracts: update mold"));
            assert!(query.matches("mold-contracts"));
            assert!(query.matches("mold\n\ncontracts"));
            assert!(query.matches("molding contracts"));
            assert!(query.matches("mold subcontracts"));
            assert!(query.matches("mold_contracts"));
            for unrelated in [
                "mold",
                "contracts",
                "manager for simplify and activity more consistent istartupservice created tracking activities",
            ] {
                assert!(!query.matches(unrelated), "{text} matched {unrelated}");
            }
        }
        assert!(Query::parse(" MÖLD\tCÔNTRACTS ").matches("côntracts / möld"));
        assert!(Query::parse("möld").matches("mölder"));
        assert!(Query::parse("a: Bob").matches("bobby smith"));
        for text in ["frag", "m: FRAG", "a: frag", "s: frag", "f: frag"] {
            let query = Query::parse(text);
            for value in ["frag", "fragment", "defrag", "defragment"] {
                assert!(query.matches(value), "{text} did not match {value}");
            }
            for value in ["flag", "f-r-a-g", "far ranging"] {
                assert!(!query.matches(value), "{text} matched {value}");
            }
        }
        assert!(Query::parse("s: dd4d723").matches("aadd4d7230000"));
        assert!(!Query::parse("s: dd4d723").matches("dd04d0723"));
        assert!(Query::parse("f: .sharpmake.cs").matches("src/project.sharpmake.cs"));
        assert!(!Query::parse("f: .sharpmake.cs").matches("src/project.sharp_extra_make.cs"));
        assert!(Query::parse("mold contracts").matches("origin/feature/mold-contracts-v2"));
    }

    #[test]
    fn partial_text_terms_filter_actual_commit_fields() {
        let (_dir, repo, input) = fixture();
        let mut index = Index::default();
        assert_eq!(run(&mut index, &repo, &input, "zebrac").rows, [2]);
        assert_eq!(run(&mut index, &repo, &input, "m: ump thin").rows, [0]);
        assert_eq!(run(&mut index, &repo, &input, "a: ób").rows, [1, 2]);
        assert_eq!(run(&mut index, &repo, &input, "ture-need").rows, [0]);
    }

    #[test]
    fn default_search_rejects_the_reported_unrelated_remote_commit() {
        let (_dir, repo, input) = fixture();
        let mut input = Arc::try_unwrap(input).ok().unwrap();
        let original = input.history.commits[0].id;
        let id = ObjectId::from_hex(b"2d068c71511a47c88509aa7514eb8ae210e9bda9").unwrap();
        let mut ghost = crate::git::commit_details(&repo, &original).unwrap();
        ghost.id = id;
        ghost.message=concat!(
            "PATCH: Add Plugin Manager related activities for the Performance Insights dashboard\n\n",
            "* `ActivityCategory` can now provide an automatic activity prefix to simplify and make activity names more consistent.\n",
            "* `IStartupService` is now created right away on program start to allow tracking of activities before Performance Insights plugin is initialized. (Stolen from @jwong2 )\n",
            "* Added a high-level category for Plugin Manager related activities (To compare before/after upcoming new plugin manager/DI initialization)"
        ).into();
        input.history.commits[0].id = id;
        input.labels.insert(id, vec!["origin/main".into()]);
        input.ghosts.insert(id, ghost);
        let input = Arc::new(input);
        let mut index = Index::default();
        assert!(
            run(&mut index, &repo, &input, "mold contracts")
                .rows
                .is_empty()
        );
        assert!(
            run(&mut index, &repo, &input, "m: mold contracts")
                .rows
                .is_empty()
        );
        assert_eq!(run(&mut index, &repo, &input, "Plugin Manager").rows, [0]);
    }

    #[test]
    fn searches_full_messages_authors_sha_refs_and_only_explicit_file_paths() {
        let (_dir, repo, input) = fixture();
        let mut index = Index::default();
        assert_eq!(run(&mut index, &repo, &input, "a: bób").rows, [1, 2]);
        assert_eq!(run(&mut index, &repo, &input, "m: Bump thingy").rows, [0]);
        assert_eq!(run(&mut index, &repo, &input, "zebracorn").rows, [2]);
        assert_eq!(run(&mut index, &repo, &input, "m: zebracorn").rows, [2]);
        assert_eq!(run(&mut index, &repo, &input, "s: ").rows.len(), 3);
        assert_eq!(
            run(
                &mut index,
                &repo,
                &input,
                &format!("s: {}", input.history.commits[1].id)
            )
            .rows,
            [1]
        );
        assert_eq!(run(&mut index, &repo, &input, "release-needle").rows, [2]);
        assert_eq!(run(&mut index, &repo, &input, "feature-needle").rows, [0]);
        assert!(
            run(&mut index, &repo, &input, "sharpmake.cs")
                .rows
                .is_empty()
        );
        assert_eq!(
            run(&mut index, &repo, &input, "f: .sharpmake.cs").rows,
            [1, 2]
        );
        assert!(run(&mut index, &repo, &input, "f: rnmd.cs").rows.is_empty());
        assert_eq!(run(&mut index, &repo, &input, "f: renamed.cs").rows, [1]);
        assert_eq!(run(&mut index, &repo, &input, "f: unrelated.txt").rows, [0]);
        assert!(index.paths.len() == 3);
        let before = index.docs.len();
        assert_eq!(
            run(&mut index, &repo, &input, "f: unrelated.txt").unavailable,
            0
        );
        assert_eq!(index.docs.len(), before);
        let latest = AtomicU64::new(2);
        index.run(
            &repo,
            Request {
                generation: 1,
                text: "a: bób".into(),
                input,
            },
            &latest,
            true,
            |_| panic!("obsolete results published"),
        );
    }

    #[test]
    fn filtered_rows_navigate_real_commits_and_escape_restores_the_graph() {
        let (_dir, repo, input) = fixture();
        let mut app = App::new();
        app.set_cached(input.history.clone(), true);
        app.list_rows = 10;
        app.handle_key(key(KeyCode::Char('/')));
        for ch in "a: bób".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        assert!(app.search.editing);
        assert!(!app.settings.open);
        assert!(!app.branches.open);
        let request = app.search_request().unwrap();
        let generation = request.generation;
        let latest = AtomicU64::new(generation);
        let mut index = Index::default();
        index.run(&repo, request, &latest, true, |r| {
            assert!(app.install_search(r));
        });
        assert_eq!(app.graph_len(), 2);
        assert_eq!(app.selected, 1);
        assert_eq!(app.history_index(1), Some(2));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.search.editing);
        assert!(app.checkout_confirmation.is_none());
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.marks, [input.history.commits[2].id]);
        app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(app.mode, Mode::Diff(_)));
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Graph));
        assert!(app.search.active());
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.search.active());
        assert_eq!(app.graph_len(), 3);
        assert_eq!(app.selected, 2);
        assert!(!app.quit);
        app.search.text.value = "m: no matching words here".into();
        app.search.changed();
        let r = run(&mut index, &repo, &input, &app.search.text.value);
        let r = Matches {
            generation: app.search.generation,
            ..r
        };
        app.install_search(r);
        assert_eq!(app.graph_len(), 0);
        assert_eq!(app.selected_id(), None);
        app.confirm_graph_checkout();
        assert!(app.checkout_confirmation.is_none());
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.graph_len(), 0);
        let r = run(&mut index, &repo, &input, "a: Alice");
        assert!(!app.install_search(r));
        app.clear_search();
        app.focus = Focus::List;
    }

    #[test]
    fn contracting_hidden_ancestry_preserves_merge_connections_without_recursion() {
        let (_dir, _repo, input) = fixture();
        let base = Base::new(input.clone());
        let projected = graph(
            &input.history,
            &base.parents,
            &[0, 2],
            &input.ghosts,
            &|| false,
        );
        let mut layout = Layout::new();
        let a = layout.push(&input.history.commits[0].id, &[input.history.commits[2].id]);
        let b = layout.push(&input.history.commits[2].id, &[]);
        assert_eq!(projected[0].0, a.cells);
        assert_eq!(projected[1].0, b.cells);
        let mut h = input.history.clone();
        let id = ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
        let mut merge = h.commits[0];
        merge.id = id;
        merge.parents = h.push_parents(&[h.commits[1].id, h.commits[2].id]);
        h.commits.insert(0, merge);
        let input = Arc::new(Input {
            history: h,
            labels: HashMap::new(),
            ghosts: HashMap::new(),
            sources: HashMap::new(),
        });
        let base = Base::new(input.clone());
        let projected = graph(
            &input.history,
            &base.parents,
            &[0, 2, 3],
            &input.ghosts,
            &|| false,
        );
        assert_eq!(
            projected[0].0[projected[0].1 as usize].kind(),
            crate::graph::layout::CellKind::MergeNode
        );
    }

    #[test]
    fn refresh_preserves_query_and_selection_and_discards_old_results() {
        let (dir, repo, input) = fixture();
        let mut app = App::new();
        app.set_cached(input.history.clone(), true);
        app.handle_key(key(KeyCode::Char('/')));
        for ch in "a: bób".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        let mut index = Index::default();
        let request = app.search_request().unwrap();
        let latest = AtomicU64::new(request.generation);
        let mut old = None;
        index.run(&repo, request, &latest, true, |r| {
            old = Some(r.clone());
            app.install_search(r);
        });
        app.select_graph(1);
        let selected = app.selected;
        app.set_refs(crate::git::snapshot_refs(&repo).unwrap());
        assert_eq!(app.search.text.value, "a: bób");
        assert!(app.search.wanted);
        assert!(!app.install_search(old.unwrap()));
        let request = app.search_request().unwrap();
        latest.store(request.generation, Ordering::Relaxed);
        let mut stages = 0;
        index.run(&repo, request, &latest, true, |r| {
            stages += 1;
            app.install_search(r);
        });
        assert_eq!(stages, 2);
        assert_eq!(app.selected, selected);
        assert_eq!(app.graph_len(), 2);
        app.scroll = 1;
        app.install_search(app.search.result.as_ref().unwrap().clone());
        assert_eq!(app.scroll, 1);
        let keep = app.selected_id().unwrap();
        git(
            dir.path(),
            &["commit", "--allow-empty", "-m", "Newer commit"],
        );
        let fresh_repo = crate::git::open(dir.path()).unwrap();
        let refs = crate::git::snapshot_refs(&fresh_repo).unwrap();
        let mut history = History::default();
        crate::git::walk(&fresh_repo, &refs.tips, &mut |h| {
            history.append(h);
            true
        })
        .unwrap();
        app.set_refs(refs);
        app.start_walk();
        app.push_batch(history);
        assert_eq!(app.history.commits[app.selected].id, keep);
        let request = app.search_request().unwrap();
        latest.store(request.generation, Ordering::Relaxed);
        index.run(&fresh_repo, request, &latest, true, |r| {
            app.install_search(r);
        });
        assert_eq!(app.selected_id(), Some(keep));
    }

    #[test]
    fn ghost_messages_are_searchable_without_objects_and_missing_paths_are_explicit() {
        let (_dir, repo, input) = fixture();
        let mut input = Arc::try_unwrap(input).ok().unwrap();
        let local = input.history.commits[0].id;
        let id = ObjectId::from_hex(b"2222222222222222222222222222222222222222").unwrap();
        let mut ghost = crate::git::commit_details(&repo, &local).unwrap();
        ghost.id = id;
        ghost.message = "Ghost message\n\nRemote body unicorn".into();
        input.history.commits[0].id = id;
        input.ghosts.insert(id, ghost);
        input.labels.insert(id, vec!["origin/ghost-needle".into()]);
        let input = Arc::new(input);
        let mut index = Index::default();
        let remote = run(&mut index, &repo, &input, "m: remote body unicorn");
        assert_eq!(remote.rows, [0]);
        assert_ne!(
            remote.graph[0].2[remote.graph[0].1 as usize] & crate::graph::dimming::GLYPH,
            0
        );
        assert_eq!(run(&mut index, &repo, &input, "ghost-needle").rows, [0]);
        let result = run(&mut index, &repo, &input, "f: unrelated.txt");
        assert_eq!(result.unavailable, 1);
        assert!(result.error.is_some());
        assert!(result.rows.is_empty());
    }

    #[test]
    fn local_file_results_arrive_before_remote_requests_and_can_cancel_them() {
        let (_dir, repo, input) = fixture();
        let mut input = Arc::try_unwrap(input).ok().unwrap();
        let remote_id = input.history.commits[0].id;
        input.sources.insert(
            remote_id,
            crate::remote::Remote::parse(
                "origin",
                "https://example.invalid/team/repo",
                Some("github"),
            )
            .unwrap(),
        );
        let input = Arc::new(input);
        let latest = AtomicU64::new(1);
        let mut index = Index::default();
        let mut local_ready = false;
        index.run(
            &repo,
            Request {
                generation: 1,
                text: "f: sharpmake.cs".into(),
                input,
            },
            &latest,
            true,
            |r| {
                if r.examined == 2 {
                    assert_eq!(r.rows, [1, 2]);
                    assert!(!r.done);
                    local_ready = true;
                    latest.store(2, Ordering::Relaxed);
                }
            },
        );
        assert!(local_ready);
        assert!(!index.docs.contains_key(&remote_id));
    }

    #[test]
    fn immutable_indexes_reload_from_disk_and_ignore_a_corrupt_record() {
        let (dir, repo, input) = fixture();
        let latest = AtomicU64::new(1);
        for text in ["m: zebracorn", "f: .sharpmake.cs"] {
            let mut index = Index::default();
            index.run(
                &repo,
                Request {
                    generation: 1,
                    text: text.into(),
                    input: input.clone(),
                },
                &latest,
                false,
                |r| assert_eq!(r.unavailable, 0),
            );
        }
        let mut paused = Index::default();
        latest.store(2, Ordering::Relaxed);
        paused.load(&repo, false, &latest, 1);
        assert!(paused.message_reader.is_some());
        assert!(!paused.loaded_messages);
        latest.store(1, Ordering::Relaxed);
        paused.load(&repo, false, &latest, 1);
        assert_eq!(paused.docs.len(), 3);
        assert!(paused.loaded_messages);
        let mut fresh = Index::default();
        fresh.load(&repo, false, &latest, 1);
        fresh.load(&repo, true, &latest, 1);
        assert_eq!(fresh.docs.len(), 3);
        assert!(
            fresh
                .docs
                .values()
                .all(|d| d.message.is_some() && d.paths.is_some())
        );
        let messages = Index::cache_path(&repo, false).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&messages)
            .unwrap();
        writeln!(file, "garbage").unwrap();
        drop(file);
        let mut fresh = Index::default();
        fresh.load(&repo, false, &latest, 1);
        assert_eq!(fresh.docs.len(), 3);
        for files in [false, true] {
            std::fs::remove_file(Index::cache_path(&repo, files).unwrap()).unwrap();
        }
        assert!(git(dir.path(), &["status", "--porcelain"]).is_empty());
    }

    #[test]
    #[ignore = "explicit large-repository search performance measurement"]
    fn benchmark_search() {
        let path = std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("repository required");
        let repo = crate::git::open(Path::new(&path)).unwrap();
        let refs = crate::git::snapshot_refs(&repo).unwrap();
        let mut history = History::default();
        crate::git::walk(&repo, &refs.tips, &mut |h| {
            history.append(h);
            true
        })
        .unwrap();
        let labels =
            refs.refs
                .into_iter()
                .fold(HashMap::<ObjectId, Vec<String>>::new(), |mut m, r| {
                    m.entry(r.id).or_default().push(r.name);
                    m
                });
        let input = Arc::new(Input {
            history,
            labels,
            ghosts: HashMap::new(),
            sources: HashMap::new(),
        });
        let mut index = Index::default();
        for (name, text) in [
            ("author", "a: Junio"),
            ("sha", "s: 6de20f6"),
            ("message", "m: fix typo"),
            ("default", "gitk"),
            ("partial", "frag"),
            ("words", "mold contracts"),
            ("files", "f: .sharpmake.cs"),
            ("files_common", "f: readme"),
        ] {
            for sample in 0..16 {
                let start = Instant::now();
                let mut ready = None;
                let mut result = None;
                let latest = AtomicU64::new(1);
                index.run(
                    &repo,
                    Request {
                        generation: 1,
                        text: text.into(),
                        input: input.clone(),
                    },
                    &latest,
                    true,
                    |r| {
                        if r.examined == r.total && ready.is_none() {
                            ready = Some(start.elapsed().as_secs_f64() * 1000.0);
                        }
                        result = Some(r);
                    },
                );
                let result = result.unwrap();
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                assert!(result.done);
                assert_eq!(result.unavailable, 0);
                println!(
                    "{}",
                    serde_json::json!({"operation":name,"sample":sample-1,"ms":ms,"matchingMs":ready.unwrap(),"matches":result.rows.len(),"commits":input.history.len(),"paths":index.paths.len(),"graphBytes":result.graph.iter().map(|(c,..)|c.len()).sum::<usize>(),"maxColumns":result.graph.iter().map(|(c,..)|c.len()).max().unwrap_or(0)})
                );
            }
        }
    }
}
