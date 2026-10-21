//! Cancellable literal filename/content search. No repository I/O on the UI thread.
use super::FileChange;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::SystemTime,
};

const MAX_RESULTS: usize = 1000;
const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
/// Byte ranges in the original text, including Unicode lowercase expansion.
pub fn highlight_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let folded = text.to_lowercase();
    if text.is_ascii() {
        return folded
            .match_indices(&query)
            .map(|(start, matched)| start..start + matched.len())
            .collect();
    }
    let mut boundaries = Vec::new();
    for (start, ch) in text.char_indices() {
        for _ in 0..ch.to_lowercase().map(char::len_utf8).sum::<usize>() {
            boundaries.push((start, start + ch.len_utf8()));
        }
    }
    folded
        .match_indices(&query)
        .filter_map(|(start, matched)| {
            Some(boundaries.get(start)?.0..boundaries.get(start + matched.len() - 1)?.1)
        })
        .collect()
}
/// A folded query of N bytes matches at most N original Unicode scalars, each
/// at most four bytes. Include that bounded overlap around a displayed span so
/// wrap-crossing matches paint without repeatedly scanning a huge source line.
fn highlight_window(
    text: &str,
    query: &str,
    visible: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    let overlap = query.trim().to_lowercase().len().saturating_mul(4);
    let mut start = visible.start.saturating_sub(overlap).min(text.len());
    let mut end = visible.end.saturating_add(overlap).min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    while !text.is_char_boundary(end) {
        end += 1;
    }
    start..end
}
/// Clip matches to a displayed byte span, including queries crossing a wrap.
pub fn visible_highlight_ranges(
    text: &str,
    query: &str,
    visible: std::ops::Range<usize>,
) -> Vec<std::ops::Range<usize>> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let window = highlight_window(text, query, &visible);
    highlight_ranges(&text[window.clone()], query)
        .into_iter()
        .filter_map(|range| {
            let start = (range.start + window.start).max(visible.start);
            let end = (range.end + window.start).min(visible.end);
            (start < end)
                .then_some(start.saturating_sub(visible.start)..end.saturating_sub(visible.start))
        })
        .collect()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub file: usize,
    pub right: bool,
    pub line: Option<u32>,
    /// UTF-8 byte offset in the displayed line, with four-column tabs expanded.
    pub offset: Option<usize>,
    pub preview: String,
}
pub struct ResultSet {
    pub sequence: u64,
    pub hits: Vec<Hit>,
    /// Complete file membership, independent of the bounded navigation list.
    pub matching_files: Vec<usize>,
    pub done: bool,
    pub skipped: usize,
    pub limited: bool,
}
struct Request {
    sequence: u64,
    root: PathBuf,
    files: Arc<Vec<FileChange>>,
    query: String,
}
struct Text {
    original: String,
    folded: String,
}
struct Cached {
    texts: Option<[Text; 2]>,
    stamp: Option<(u64, SystemTime)>,
}
impl Cached {
    fn bytes(&self) -> usize {
        self.texts.as_ref().map_or(0, |t| {
            t.iter().map(|s| s.original.len() + s.folded.len()).sum()
        })
    }
}
#[derive(Default)]
struct Index {
    root: PathBuf,
    files: Arc<Vec<FileChange>>,
    content: Vec<Option<Cached>>,
    bytes: usize,
}
impl Index {
    fn prepare(&mut self, root: &PathBuf, files: &Arc<Vec<FileChange>>) {
        if self.root != *root || !Arc::ptr_eq(&self.files, files) {
            self.root = root.clone();
            self.files = files.clone();
            self.content = (0..files.len()).map(|_| None).collect();
            self.bytes = 0;
        }
    }
    fn load(&mut self, file: usize, repo: &gix::Repository) {
        let change = &self.files[file];
        let stamp = change
            .worktree
            .then(|| {
                std::fs::symlink_metadata(self.root.join(&change.path))
                    .ok()
                    .and_then(|m| m.modified().ok().map(|time| (m.len(), time)))
            })
            .flatten();
        if self.content[file]
            .as_ref()
            .is_some_and(|c| !change.worktree || c.stamp == stamp)
        {
            return;
        }
        if let Some(old) = self.content[file].take() {
            self.bytes = self.bytes.saturating_sub(old.bytes());
        }
        let texts = super::external_contents(repo, change)
            .ok()
            .and_then(|(left, right)| {
                if left
                    .iter()
                    .take(8000)
                    .chain(right.iter().take(8000))
                    .any(|b| *b == 0)
                {
                    return None;
                }
                // Reserve before case folding; recheck actual allocation for Unicode expansion.
                if self.bytes + (left.len() + right.len()) * 2 > MAX_INDEX_BYTES {
                    return None;
                }
                let text = |bytes: Vec<u8>| {
                    let original = String::from_utf8_lossy(&bytes).into_owned();
                    let folded = original.to_lowercase();
                    Text { original, folded }
                };
                let texts = [text(left), text(right)];
                let bytes = texts
                    .iter()
                    .map(|t| t.original.len() + t.folded.len())
                    .sum::<usize>();
                (self.bytes + bytes <= MAX_INDEX_BYTES).then_some(texts)
            });
        let entry = Cached { texts, stamp };
        self.bytes += entry.bytes();
        self.content[file] = Some(entry);
    }
}

pub struct Worker {
    tx: mpsc::Sender<Request>,
    pub rx: mpsc::Receiver<ResultSet>,
    generation: Arc<AtomicU64>,
}
impl Default for Worker {
    fn default() -> Self {
        Self::new(None)
    }
}
impl Worker {
    pub fn with_notify(notify: async_channel::Sender<()>) -> Self {
        Self::new(Some(notify))
    }
    fn new(notify: Option<async_channel::Sender<()>>) -> Self {
        let (tx, requests) = mpsc::channel::<Request>();
        let (results, rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        std::thread::Builder::new()
            .name("diff-search".into())
            .spawn(move || {
                let mut index = Index::default();
                while let Ok(mut request) = requests.recv() {
                    for newer in requests.try_iter() {
                        request = newer;
                    }
                    let sequence = request.sequence;
                    let cancelled = || current.load(Ordering::Relaxed) != sequence;
                    if cancelled() {
                        continue;
                    }
                    index.prepare(&request.root, &request.files);
                    let query = request.query.trim().to_lowercase();
                    if query.is_empty() {
                        continue;
                    }
                    let mut hits = Vec::new();
                    let mut matching_files = vec![false; index.files.len()];
                    for (file, change) in index.files.iter().enumerate() {
                        if cancelled() {
                            break;
                        }
                        if change.path.to_lowercase().contains(&query)
                            || change
                                .old_path
                                .as_ref()
                                .is_some_and(|p| p.to_lowercase().contains(&query))
                        {
                            matching_files[file] = true;
                            if hits.len() < MAX_RESULTS {
                                hits.push(Hit {
                                    file,
                                    right: true,
                                    line: None,
                                    offset: None,
                                    preview: "Filename match".into(),
                                });
                            }
                        }
                    }
                    if cancelled() {
                        continue;
                    }
                    let _ = results.send(ResultSet {
                        sequence,
                        hits: hits.clone(),
                        matching_files: matching_files
                            .iter()
                            .enumerate()
                            .filter_map(|(file, matched)| matched.then_some(file))
                            .collect(),
                        done: false,
                        skipped: 0,
                        limited: hits.len() == MAX_RESULTS,
                    });
                    if let Some(notify) = &notify {
                        let _ = notify.try_send(());
                    }
                    let repo = crate::git::open(&request.root);
                    let mut skipped = 0;
                    if let Ok(repo) = repo {
                        for (file, membership) in matching_files.iter_mut().enumerate() {
                            if cancelled() {
                                break;
                            }
                            index.load(file, &repo);
                            let Some(texts) =
                                index.content[file].as_ref().and_then(|c| c.texts.as_ref())
                            else {
                                skipped += 1;
                                continue;
                            };
                            let matched = if hits.len() < MAX_RESULTS {
                                scan(file, texts, &query, &mut hits, &cancelled)
                            } else {
                                texts.iter().any(|text| {
                                    text.folded
                                        .lines()
                                        .enumerate()
                                        .take_while(|(line, _)| line % 128 != 0 || !cancelled())
                                        .any(|(_, content)| content.contains(&query))
                                })
                            };
                            *membership |= matched;
                        }
                    } else {
                        skipped = index.files.len();
                    }
                    if !cancelled() {
                        let limited = hits.len() == MAX_RESULTS;
                        let _ = results.send(ResultSet {
                            sequence,
                            hits,
                            matching_files: matching_files
                                .iter()
                                .enumerate()
                                .filter_map(|(file, matched)| matched.then_some(file))
                                .collect(),
                            done: true,
                            skipped,
                            limited,
                        });
                        if let Some(notify) = &notify {
                            let _ = notify.try_send(());
                        }
                    }
                }
            })
            .expect("start diff search worker");
        Self { tx, rx, generation }
    }
}
impl Worker {
    pub fn search(&self, root: PathBuf, files: Arc<Vec<FileChange>>, query: String) -> u64 {
        let sequence = self.cancel();
        let _ = self.tx.send(Request {
            sequence,
            root,
            files,
            query,
        });
        sequence
    }
    pub fn cancel(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn scan(
    file: usize,
    texts: &[Text; 2],
    query: &str,
    hits: &mut Vec<Hit>,
    cancelled: &impl Fn() -> bool,
) -> bool {
    let mut matched = false;
    for (side, text) in texts.iter().enumerate() {
        // Unchanged complete files do not need duplicate before/after results.
        if side == 0 && text.original == texts[1].original {
            continue;
        }
        for (line, (folded, original)) in text.folded.lines().zip(text.original.lines()).enumerate()
        {
            if line % 128 == 0 && cancelled() {
                return matched;
            }
            let mut matches = folded.match_indices(query).peekable();
            if matches.peek().is_none() {
                continue;
            }
            let plain_ascii = original.is_ascii() && !original.contains('\t');
            let mut positions = original.char_indices();
            let (mut folded_bytes, mut displayed_bytes, mut displayed_columns) = (0, 0, 0);
            let (mut original_hit, mut displayed_hit) = (0, 0);
            for (offset, _) in matches {
                if hits.len().is_multiple_of(128) && cancelled() {
                    return matched;
                }
                if plain_ascii {
                    original_hit = offset;
                    displayed_hit = offset;
                } else {
                    // Sorted matches share a single forward mapping pass, even
                    // when Unicode lowercase expands or tabs precede a match.
                    while folded_bytes <= offset {
                        let Some((index, ch)) = positions.next() else {
                            break;
                        };
                        original_hit = index;
                        displayed_hit = displayed_bytes;
                        folded_bytes += if ch.is_ascii() {
                            1
                        } else {
                            ch.to_lowercase().map(char::len_utf8).sum()
                        };
                        if ch == '\t' {
                            let spaces = 4 - displayed_columns % 4;
                            displayed_bytes += spaces;
                            displayed_columns += spaces;
                        } else {
                            displayed_bytes += ch.len_utf8();
                            displayed_columns += 1;
                        }
                    }
                }
                matched = true;
                hits.push(Hit {
                    file,
                    right: side == 1,
                    line: Some((line + 1) as u32),
                    offset: Some(displayed_hit),
                    preview: match_preview(original, original_hit),
                });
                if hits.len() == MAX_RESULTS {
                    return true;
                }
            }
        }
    }
    matched
}

/// `hit` is a byte boundary in the original, non-folded line.
fn match_preview(original: &str, hit: usize) -> String {
    // Keep the match near the leading edge: results live in a narrow sidebar.
    let start = original[..hit]
        .char_indices()
        .rev()
        .nth(15)
        .map_or(0, |(index, _)| index);
    let tail = &original[start..];
    let end = tail
        .char_indices()
        .nth(200)
        .map_or(tail.len(), |(index, _)| index);
    let mut preview = String::with_capacity(end + 6);
    if start != 0 {
        preview.push('…');
    }
    preview.push_str(&tail[..end]);
    if end < tail.len() {
        preview.push('…');
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};
    #[test]
    fn highlight_offsets_follow_original_unicode_and_multiple_matches() {
        let text = "İ CAFÉ café";
        let ranges = highlight_ranges(text, "café");
        assert_eq!(
            ranges.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(),
            ["CAFÉ", "café"]
        );
        assert!(highlight_ranges(text, " ").is_empty());
        assert_eq!(&text[highlight_ranges(text, "i")[0].clone()], "İ");
    }
    #[test]
    fn scanner_navigates_each_occurrence_and_maps_unicode_and_tabs_once() {
        for (original, query) in [
            ("needle and needle", "needle"),
            ("İ\tCAFÉ and café", "café"),
        ] {
            let mut hits = Vec::new();
            scan(0, &texts("", original), query, &mut hits, &|| false);
            assert_eq!(hits.len(), 2);
            assert_eq!(hits[0].line, hits[1].line);
            assert!(hits[1].offset > hits[0].offset);
            let displayed = super::super::sidebyside::decode(original.as_bytes());
            let expected = highlight_ranges(&displayed, query);
            assert_eq!(
                hits.iter()
                    .map(|hit| hit.offset.unwrap())
                    .collect::<Vec<_>>(),
                expected.iter().map(|range| range.start).collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn matches_crossing_wrap_boundaries_highlight_both_visible_segments() {
        let text = "prefix CAFÉ needle suffix";
        let boundary = text.find("needle").unwrap();
        let left = visible_highlight_ranges(text, "café needle", 0..boundary);
        let right = visible_highlight_ranges(text, "café needle", boundary..text.len());
        assert_eq!(&text[left[0].clone()], "CAFÉ ");
        assert_eq!(
            &text[boundary + right[0].start..boundary + right[0].end],
            "needle"
        );
        assert!(visible_highlight_ranges(text, "CAFÉ", boundary..text.len()).is_empty());
    }
    #[test]
    fn wrapped_highlight_work_is_bounded_by_visible_span_and_query() {
        let text = "界".repeat(1_400_000);
        let visible = 2_100_000..2_100_300;
        let window = highlight_window(&text, "界界", &visible);
        assert!(text.is_char_boundary(window.start) && text.is_char_boundary(window.end));
        assert!(window.len() <= visible.len() + 8 * "界界".len() + 6);
        let matches = visible_highlight_ranges(&text, "界界", visible.clone());
        assert!(!matches.is_empty());
        assert!(matches.iter().all(|range| range.end <= visible.len()));
    }
    #[test]
    fn navigation_cap_does_not_hide_later_content_or_filename_membership() {
        let (dir, files) = fixture();
        let source = files.iter().find(|f| f.path == "source.txt").unwrap();
        let mut many = Vec::new();
        for i in 0..MAX_RESULTS + 2 {
            let mut file = source.clone();
            file.path = format!("needle-{i}.txt");
            many.push(file);
        }
        many.push(source.clone());
        let worker = Worker::default();
        let sequence = worker.search(dir.path().into(), Arc::new(many), "needle".into());
        loop {
            let result = worker.rx.recv_timeout(Duration::from_secs(15)).unwrap();
            if result.sequence == sequence && result.done {
                assert_eq!(result.hits.len(), MAX_RESULTS);
                assert!(result.limited);
                assert_eq!(result.matching_files.len(), MAX_RESULTS + 3);
                assert_eq!(result.matching_files.last(), Some(&(MAX_RESULTS + 2)));
                break;
            }
        }
    }
    fn git(root: &Path, args: &[&str]) -> String {
        String::from_utf8(crate::git::command::checked(root, args).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }
    fn fixture() -> (tempfile::TempDir, Arc<Vec<FileChange>>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Search"]);
        git(root, &["config", "user.email", "search@example.test"]);
        git(root, &["config", "commit.gpgSign", "false"]);
        std::fs::write(root.join("source.txt"), "OldNeedle\ncommon\n").unwrap();
        std::fs::write(root.join("needle-name.txt"), "before\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "before"]);
        let old = gix::ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        std::fs::write(root.join("source.txt"), "NewNeedle\ncommon\n").unwrap();
        std::fs::write(root.join("needle-name.txt"), "after\n").unwrap();
        std::fs::write(root.join("binary.bin"), b"binary\0needle").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "after"]);
        let new = gix::ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        let repo = crate::git::open(root).unwrap();
        let files = Arc::new(
            super::super::tree_diff(&repo, Some(&old), &new)
                .unwrap()
                .files,
        );
        (dir, files)
    }
    fn finish(worker: &Worker, sequence: u64) -> (Vec<Hit>, usize, bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut saw_partial = false;
        loop {
            let result = worker
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("search result before deadline");
            if result.sequence != sequence {
                continue;
            }
            if result.done {
                assert!(saw_partial, "filenames stream before content completion");
                return (result.hits, result.skipped, result.limited);
            }
            saw_partial = true;
        }
    }
    #[test]
    fn real_worker_streams_filenames_and_both_object_sides_then_reuses_hot_cache() {
        let (dir, files) = fixture();
        let worker = Worker::default();
        let sequence = worker.search(dir.path().into(), files.clone(), "NEEDLE".into());
        let (cold, skipped, limited) = finish(&worker, sequence);
        assert!(!limited);
        assert_eq!(skipped, 1, "binary content is visibly skipped");
        let source = files
            .iter()
            .position(|file| file.path == "source.txt")
            .unwrap();
        assert!(
            cold.iter()
                .any(|hit| hit.line.is_none() && files[hit.file].path == "needle-name.txt")
        );
        assert!(
            cold.iter()
                .any(|hit| hit.file == source && !hit.right && hit.line == Some(1))
        );
        assert!(
            cold.iter()
                .any(|hit| hit.file == source && hit.right && hit.line == Some(1))
        );
        let sequence = worker.search(dir.path().into(), files, "needle".into());
        let (hot, hot_skipped, _) = finish(&worker, sequence);
        assert_eq!(hot, cold);
        assert_eq!(hot_skipped, skipped);
    }
    #[test]
    fn live_index_reloads_changed_file_and_new_file_set_invalidates_cache() {
        let (dir, _) = fixture();
        let root = dir.path();
        std::fs::write(root.join("source.txt"), "live first needle\n").unwrap();
        let repo = crate::git::open(root).unwrap();
        let files = Arc::new(crate::git::worktree::snapshot(&repo).unwrap().files);
        let file = files
            .iter()
            .position(|file| file.path == "source.txt")
            .unwrap();
        let mut index = Index::default();
        index.prepare(&root.into(), &files);
        index.load(file, &repo);
        assert!(
            index.content[file]
                .as_ref()
                .unwrap()
                .texts
                .as_ref()
                .unwrap()[1]
                .original
                .contains("first")
        );
        let first_bytes = index.bytes;
        std::fs::write(
            root.join("source.txt"),
            "live second needle with different size\n",
        )
        .unwrap();
        index.load(file, &repo);
        assert!(
            index.content[file]
                .as_ref()
                .unwrap()
                .texts
                .as_ref()
                .unwrap()[1]
                .original
                .contains("second")
        );
        assert_ne!(index.bytes, first_bytes);
        let replacement = Arc::new((*files).clone());
        index.prepare(&root.into(), &replacement);
        assert_eq!(index.bytes, 0);
        assert!(index.content.iter().all(Option::is_none));
    }
    #[test]
    fn queued_cancel_cannot_replace_the_newest_worker_result() {
        let (dir, files) = fixture();
        let worker = Worker::default();
        let obsolete = worker.search(dir.path().into(), files.clone(), "oldneedle".into());
        worker.cancel();
        let current = worker.search(dir.path().into(), files, "newneedle".into());
        assert!(current > obsolete);
        let (hits, _, _) = finish(&worker, current);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].right);
        assert_eq!(hits[0].preview, "NewNeedle");
    }
    #[test]
    #[ignore = "one warmup and15 cold/hot real worker content searches across32 files"]
    fn benchmark_worker_cold_hot() {
        let (dir, _) = fixture();
        let content = (0..5000)
            .map(|line| format!("ordinary source line {line} with identifiers and values\n"))
            .collect::<String>()
            + "unique search target\n";
        for file in 0..32 {
            std::fs::write(dir.path().join(format!("search-{file}.txt")), &content).unwrap();
        }
        let repo = crate::git::open(dir.path()).unwrap();
        let files = Arc::new(crate::git::worktree::snapshot(&repo).unwrap().files);
        for sample in -1..15 {
            let worker = Worker::default();
            let start = Instant::now();
            let sequence = worker.search(
                dir.path().into(),
                files.clone(),
                "unique search target".into(),
            );
            let (cold, skipped, limited) = finish(&worker, sequence);
            let cold_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(cold.len(), 32);
            assert_eq!(skipped, 0);
            assert!(!limited);
            let start = Instant::now();
            let sequence = worker.search(
                dir.path().into(),
                files.clone(),
                "unique search target".into(),
            );
            let (hot, _, _) = finish(&worker, sequence);
            assert_eq!(hot, cold);
            println!(
                "{}",
                serde_json::json!({"sample":sample,"files":files.len(),"bytes":content.len()*32,"coldMs":cold_ms,"hotMs":start.elapsed().as_secs_f64()*1000.})
            );
        }
    }
    fn texts(a: &str, b: &str) -> [Text; 2] {
        [a, b].map(|s| Text {
            original: s.into(),
            folded: s.to_lowercase(),
        })
    }
    #[test]
    fn literal_unicode_both_sides_and_cancellation() {
        let mut hits = vec![];
        scan(
            3,
            &texts("before\nCAFÉ\n", "after\ncafé\n"),
            "café",
            &mut hits,
            &|| false,
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(
            (hits[0].file, hits[0].line, hits[0].right),
            (3, Some(2), false)
        );
        assert!(hits[1].right);
        hits.clear();
        scan(0, &texts("same", "same"), "same", &mut hits, &|| false);
        assert_eq!(hits.len(), 1);
        hits.clear();
        scan(0, &texts("same", "same"), "same", &mut hits, &|| true);
        assert!(hits.is_empty());
    }
    #[test]
    fn result_count_is_bounded() {
        for content in ["needle\n".repeat(5000), "needle ".repeat(5000)] {
            let mut hits = vec![];
            scan(0, &texts("", &content), "needle", &mut hits, &|| false);
            assert_eq!(hits.len(), MAX_RESULTS);
        }
    }
    #[test]
    fn long_line_preview_centers_match_and_maps_unicode_lowercase_offsets() {
        for prefix in ["x".repeat(500), "İ".repeat(250), "ẞ".repeat(250)] {
            let original = format!("{prefix} marker CAFÉ {}", "z".repeat(500));
            let offset = original.find("CAFÉ").unwrap();
            let preview = match_preview(&original, offset);
            assert!(preview.starts_with('…') && preview.ends_with('…'));
            assert!(preview.contains("marker CAFÉ"));
            assert_eq!(preview.chars().count(), 202);
            let mut hits = Vec::new();
            scan(0, &texts("", &original), "café", &mut hits, &|| false);
            assert_eq!(hits[0].preview, preview);
        }
    }
    #[test]
    fn notified_worker_signals_partial_and_complete_result_delivery() {
        let (dir, files) = fixture();
        let (tx, rx) = async_channel::unbounded();
        let worker = Worker::with_notify(tx);
        let sequence = worker.search(dir.path().into(), files, "needle".into());
        let wait_notification = || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while rx.try_recv().is_err() {
                assert!(Instant::now() < deadline, "worker notification timed out");
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        wait_notification();
        let (hits, _, _) = finish(&worker, sequence);
        assert!(!hits.is_empty());
        wait_notification();
    }
    #[test]
    #[ignore]
    fn benchmark_search() {
        let content = (0..100_000)
            .map(|i| format!("line {i}: ordinary source with data and identifiers\n"))
            .collect::<String>();
        let text = texts("", &content);
        for sample in 0..16 {
            let start = std::time::Instant::now();
            let mut hits = vec![];
            scan(0, &text, "line 99999:", &mut hits, &|| false);
            assert_eq!(hits.len(), 1);
            eprintln!(
                "diff_search sample={sample} cached_ms={:.6} bytes={}",
                start.elapsed().as_secs_f64() * 1000.,
                content.len()
            );
        }
    }
}
