#![allow(dead_code)] // Legacy adapters remain as regression-test references.
//! Branch browser state and bounded worker-only local activity scoring.
use crate::git::{RefKind, RefSnapshot};
use crate::remote;
use crate::term::{KeyCode, KeyEvent};
use gix::ObjectId;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub name: String,
    pub remote: Option<String>,
    /// Present for an API branch; guards against a reconfigured remote at checkout.
    pub url: Option<String>,
    pub sha: String,
}

impl Target {
    pub fn label(&self) -> String {
        match &self.remote {
            Some(r) => format!("{r}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

pub struct Entry {
    pub target: Target,
    pub current: bool,
    search: String,
    pub activity: Option<Activity>,
}

#[derive(Clone, Debug)]
pub struct Activity {
    pub author: String,
    pub author_time: i64,
    pub commit_time: i64,
    pub recent: u32,
    /// Only the last 100 first-parent commits are examined; missing ancestry
    /// also makes the count a lower bound.
    pub limited: bool,
}

pub const WEEK: i64 = 7 * 86_400;

/// Bounded first-parent scoring: merges do not multiply shared side histories.
/// Called on a worker, once per set of local branch tips.
pub fn local_activity(
    repo: &gix::Repository,
    tips: &[Target],
    now: i64,
) -> Vec<(Target, Activity)> {
    let mut known = HashMap::new();
    tips.iter()
        .filter_map(|target| {
            let id = ObjectId::from_hex(target.sha.as_bytes()).ok()?;
            let tip = crate::git::commit_details(repo, &id).ok()?;
            let mut next = Some(id);
            let mut recent = 0;
            let mut visited = HashSet::new();
            for _ in 0..100 {
                let Some(id) = next else { break };
                if !visited.insert(id) {
                    break;
                }
                if let std::collections::hash_map::Entry::Vacant(entry) = known.entry(id) {
                    let Ok(details) = crate::git::commit_details(repo, &id) else {
                        break;
                    };
                    entry.insert(details);
                }
                let c = &known[&id];
                recent += u32::from(c.commit_time >= now - WEEK && c.commit_time <= now);
                next = c.parents.first().copied();
            }
            Some((
                target.clone(),
                Activity {
                    author: tip.author_name,
                    author_time: tip.author_time,
                    commit_time: tip.commit_time,
                    recent,
                    limited: next.is_some(),
                },
            ))
        })
        .collect()
}

#[derive(Default)]
pub struct Browser {
    pub open: bool,
    pub entries: Vec<Entry>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
    follow_top: bool,
    pub rows: usize,
    pub query: String,
    pub loading: bool,
    pub busy: bool,
    pub status: String,
    pub errors: Vec<String>,
    pub want_refresh: Option<bool>,
    pub jump: Option<ObjectId>,
    pub preview_targets: Vec<Target>,
    pub preview_requested: bool,
    pub pending_jump: Option<ObjectId>,
    pub activity_cache: HashMap<Target, Activity>,
    pub activity_attempted: HashSet<Target>,
    pub activity_errors: HashMap<Target, String>,
    pub activity_requests: usize,
    pub remotes: BTreeMap<String, remote::Update>,
    pub last_check: Option<std::time::Instant>,
}

impl Browser {
    pub fn show(&mut self, refs: Option<&RefSnapshot>) {
        self.open = true;
        self.reset_to_top();
        self.rebuild(refs);
        if !self.loading
            && (self.remotes.values().any(|u| u.scoped)
                || self
                    .last_check
                    .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(300)))
        {
            self.want_refresh = Some(false);
        }
    }

    pub fn update(&mut self, update: remote::Update, refs: Option<&RefSnapshot>) {
        self.remotes.insert(update.remote.name.clone(), update);
        self.rebuild(refs);
    }

    pub fn rebuild(&mut self, refs: Option<&RefSnapshot>) {
        let keep = self
            .chosen()
            .map(|e| (e.target.remote.clone(), e.target.name.clone()));
        let mut entries = BTreeMap::new();
        if let Some(refs) = refs {
            for r in &refs.refs {
                let target = match r.kind {
                    RefKind::Branch | RefKind::Head
                        if refs.head_branch.as_ref() == Some(&r.name)
                            || r.kind == RefKind::Branch =>
                    {
                        Target {
                            name: r.name.clone(),
                            remote: None,
                            url: None,
                            sha: r.id.to_string(),
                        }
                    }
                    RefKind::Remote => {
                        let Some((remote, name)) = r.name.split_once('/') else {
                            continue;
                        };
                        if name == "HEAD" {
                            continue;
                        }
                        Target {
                            name: name.into(),
                            remote: Some(remote.into()),
                            url: None,
                            sha: r.id.to_string(),
                        }
                    }
                    _ => continue,
                };
                let current =
                    target.remote.is_none() && refs.head_branch.as_ref() == Some(&target.name);
                let search = target.label().to_lowercase();
                entries.insert(
                    (target.remote.clone(), target.name.clone()),
                    Entry {
                        target,
                        current,
                        search,
                        activity: None,
                    },
                );
            }
        }
        for (name, update) in &self.remotes {
            // A successful complete list is authoritative, including branch deletions.
            if !update.stale && !update.scoped {
                entries.retain(|(remote, _), _| remote.as_ref() != Some(name));
            }
            for branch in &update.branches {
                let target = Target {
                    name: branch.name.clone(),
                    remote: Some(name.clone()),
                    url: Some(update.remote.url.clone()),
                    sha: branch.sha.clone(),
                };
                let search = target.label().to_lowercase();
                entries.insert(
                    (target.remote.clone(), target.name.clone()),
                    Entry {
                        target,
                        current: false,
                        search,
                        activity: None,
                    },
                );
            }
        }
        self.entries = entries.into_values().collect();
        let mut by_sha: HashMap<&str, &Activity> = HashMap::new();
        for (target, activity) in &self.activity_cache {
            let keep = by_sha.entry(&target.sha).or_insert(activity);
            if activity.recent > keep.recent {
                *keep = activity;
            }
        }
        for e in &mut self.entries {
            e.activity = self
                .activity_cache
                .get(&e.target)
                .or_else(|| by_sha.get(e.target.sha.as_str()).copied())
                .cloned();
        }
        self.entries.sort_by(|a, b| {
            let rank = |e: &Entry| e.activity.as_ref().map(|a| (a.recent, a.commit_time));
            rank(b)
                .cmp(&rank(a))
                .then_with(|| a.target.remote.cmp(&b.target.remote))
                .then_with(|| a.target.name.cmp(&b.target.name))
        });
        self.filter();
        if self.follow_top {
            self.selected = 0;
            self.scroll = 0;
        } else if let Some(keep) = keep
            && let Some(i) = self.visible.iter().position(|&i| {
                (
                    self.entries[i].target.remote.clone(),
                    self.entries[i].target.name.clone(),
                ) == keep
            })
        {
            self.selected = i;
        }
        self.ensure_visible();
    }

    pub fn filter(&mut self) {
        let q = self.query.to_lowercase();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.search.contains(&q))
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn chosen(&self) -> Option<&Entry> {
        self.visible.get(self.selected).map(|&i| &self.entries[i])
    }

    pub fn install_activity(
        &mut self,
        values: Vec<(Target, Activity)>,
        refs: Option<&RefSnapshot>,
    ) {
        self.activity_cache.extend(values);
        self.rebuild(refs);
    }

    pub fn next_remote_activity(&mut self) -> Option<(Target, remote::Remote)> {
        let entry = self
            .chosen()
            .into_iter()
            .chain(
                self.visible
                    .iter()
                    .skip(self.scroll)
                    .take(self.rows.max(1))
                    .map(|&i| &self.entries[i]),
            )
            .chain(self.entries.iter().filter(|_| self.activity_requests < 100))
            .find(|e| {
                e.target.url.is_some()
                    && e.activity.is_none()
                    && !self.activity_attempted.contains(&e.target)
            })?;
        let target = entry.target.clone();
        let remote = self.remotes.get(target.remote.as_ref()?)?.remote.clone();
        self.activity_attempted.insert(target.clone());
        self.activity_requests += 1;
        Some((target, remote))
    }

    pub fn ensure_visible(&mut self) {
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + self.rows.max(1) {
            self.scroll = self.selected + 1 - self.rows.max(1);
        }
        self.scroll = self
            .scroll
            .min(self.visible.len().saturating_sub(self.rows.max(1)));
    }

    fn reset_to_top(&mut self) {
        self.follow_top = true;
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn select(&mut self, index: usize) {
        self.follow_top = false;
        self.selected = index.min(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn move_by(&mut self, delta: isize) {
        self.select(self.selected.saturating_add_signed(delta));
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.ctrl) {
            (KeyCode::Esc, _) => self.open = false,
            (KeyCode::Up, _) => self.move_by(-1),
            (KeyCode::Down, _) => self.move_by(1),
            (KeyCode::PageUp, _) => self.move_by(-(self.rows.max(1) as isize)),
            (KeyCode::PageDown, _) => self.move_by(self.rows.max(1) as isize),
            (KeyCode::Home, _) => {
                self.reset_to_top();
                self.ensure_visible();
            }
            (KeyCode::End, _) => {
                self.select(usize::MAX);
            }
            (KeyCode::F(5), _) | (KeyCode::Char('r'), true) if !self.loading && !self.busy => {
                self.want_refresh = Some(true)
            }
            (KeyCode::Tab, _) => {
                self.jump = self
                    .chosen()
                    .and_then(|e| ObjectId::from_hex(e.target.sha.as_bytes()).ok());
            }
            (KeyCode::Backspace, _) => {
                self.query.pop();
                self.reset_to_top();
                self.filter();
            }
            (KeyCode::Char('u'), true) => {
                self.query.clear();
                self.reset_to_top();
                self.filter();
            }
            (KeyCode::Char(c), false) if !key.alt && !c.is_control() && self.query.len() < 256 => {
                self.query.push(c);
                self.reset_to_top();
                self.filter();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{RefEntry, RefKind};

    #[test]
    fn opening_stays_at_top_during_activity_updates_until_the_user_navigates() {
        let refs = RefSnapshot {
            refs: (1..=20)
                .map(|i| RefEntry {
                    name: format!("branch-{i:02}"),
                    id: ObjectId::from_hex(format!("{i:040x}").as_bytes()).unwrap(),
                    kind: RefKind::Branch,
                })
                .collect(),
            ..Default::default()
        };
        let mut browser = Browser {
            rows: 3,
            ..Default::default()
        };
        browser.show(Some(&refs));
        let scores = |entries: &[Entry], reverse: bool| {
            entries
                .iter()
                .map(|e| {
                    let rank = e.target.name[7..].parse::<u32>().unwrap();
                    (
                        e.target.clone(),
                        Activity {
                            author: "Author".into(),
                            author_time: 1000,
                            commit_time: 1000,
                            recent: if reverse { 21 - rank } else { rank },
                            limited: false,
                        },
                    )
                })
                .collect()
        };
        browser.install_activity(scores(&browser.entries, false), Some(&refs));
        assert_eq!(browser.chosen().unwrap().target.name, "branch-20");
        assert_eq!((browser.selected, browser.scroll), (0, 0));
        browser.move_by(4);
        let chosen = browser.chosen().unwrap().target.clone();
        browser.install_activity(scores(&browser.entries, true), Some(&refs));
        assert_eq!(browser.chosen().unwrap().target, chosen);
        assert!(browser.scroll > 0);
        browser.open = false;
        browser.show(Some(&refs));
        assert_eq!(browser.chosen().unwrap().target.name, "branch-01");
        assert_eq!((browser.selected, browser.scroll), (0, 0));
        browser.install_activity(scores(&browser.entries, false), Some(&refs));
        assert_eq!((browser.selected, browser.scroll), (0, 0));
        // Mouse clicks use select(), and must retain that branch during updates too.
        browser.select(5);
        let clicked = browser.chosen().unwrap().target.clone();
        browser.rebuild(Some(&refs));
        assert_eq!(browser.chosen().unwrap().target, clicked);
    }

    #[test]
    fn activity_orders_branches_and_preserves_selection_filter_and_rendering() {
        let id = |s: &str| ObjectId::from_hex(s.repeat(40).as_bytes()).unwrap();
        let refs = RefSnapshot {
            refs: vec![
                RefEntry {
                    name: "alphabetical".into(),
                    id: id("a"),
                    kind: RefKind::Branch,
                },
                RefEntry {
                    name: "busy".into(),
                    id: id("b"),
                    kind: RefKind::Branch,
                },
                RefEntry {
                    name: "latest".into(),
                    id: id("c"),
                    kind: RefKind::Branch,
                },
            ],
            ..Default::default()
        };
        let mut browser = Browser {
            rows: 10,
            ..Default::default()
        };
        browser.rebuild(Some(&refs));
        let chosen = browser.chosen().unwrap().target.clone();
        let values = browser
            .entries
            .iter()
            .map(|e| {
                (
                    e.target.clone(),
                    Activity {
                        author: "Graph Author With Long Name".into(),
                        author_time: 1000,
                        commit_time: if e.target.name == "latest" {
                            2000
                        } else {
                            1000
                        },
                        recent: if e.target.name == "alphabetical" {
                            1
                        } else {
                            4
                        },
                        limited: false,
                    },
                )
            })
            .collect();
        browser.install_activity(values, Some(&refs));
        assert_eq!(
            browser
                .entries
                .iter()
                .map(|e| e.target.name.as_str())
                .collect::<Vec<_>>(),
            ["latest", "busy", "alphabetical"]
        );
        assert_eq!(browser.chosen().unwrap().target, chosen);
        let mut screen = crate::term::Screen::new(120, 20);
        crate::ui::branches::draw(&mut browser, &mut screen, "test", 1000 + 3 * 3600);
        let text: String = screen.test_cells().iter().map(|c| c.ch).collect();
        assert!(text.contains("Graph Author With Long Name"));
        assert!(!text.contains("[local]"));
        assert!(!text.contains("Activity:"));
        assert!(text.contains("3h"));
        assert!(text.contains("4 /wk"));
        assert!(
            screen
                .test_cells()
                .iter()
                .any(|c| c.ch == 'G' && c.style.fg == crate::ui::theme::FG_AUTHOR)
        );
        assert!(
            screen
                .test_cells()
                .iter()
                .any(|c| c.ch == '3' && c.style.fg == crate::ui::theme::FG_DATE)
        );
        browser.query = "busy".into();
        browser.rebuild(Some(&refs));
        assert_eq!(browser.visible.len(), 1);
        assert_eq!(browser.chosen().unwrap().target.name, "busy");
        for w in [20, 49, 50, 69, 70] {
            crate::ui::branches::draw(
                &mut browser,
                &mut crate::term::Screen::new(w, 10),
                "test",
                1000,
            );
        }
        let remote = remote::Remote::parse("origin", "https://github.com/git/git", None).unwrap();
        for (cached, stale) in [(false, false), (true, false), (true, true)] {
            browser.update(
                remote::Update {
                    remote: remote.clone(),
                    branches: vec![remote::Branch {
                        name: "busy".into(),
                        sha: id("b").to_string(),
                    }],
                    scoped: false,
                    cached,
                    stale,
                },
                Some(&refs),
            );
            crate::ui::branches::draw(&mut browser, &mut screen, "test", 1000);
            let text: String = screen.test_cells().iter().map(|c| c.ch).collect();
            assert!(text.contains("origin/busy"));
            assert!(!text.contains("cached") && !text.contains("stale") && !text.contains("live"));
        }
    }

    #[test]
    #[ignore = "explicit large-repository branch performance measurement"]
    fn benchmark_browser() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("repository required"),
        );
        let repo = crate::git::open(&path).unwrap();
        let mut refs = crate::git::snapshot_refs(&repo).unwrap();
        if std::env::var("GIT_RUSTLE_BENCH_BRANCHES").as_deref() == Ok("1000") {
            let ids =
                crate::git::command::checked(&path, &["rev-list", "--all", "--max-count=1000"])
                    .unwrap();
            refs.refs = String::from_utf8(ids)
                .unwrap()
                .lines()
                .enumerate()
                .map(|(i, id)| RefEntry {
                    name: format!("synthetic-{i:04}"),
                    id: ObjectId::from_hex(id.as_bytes()).unwrap(),
                    kind: RefKind::Branch,
                })
                .collect();
            assert_eq!(refs.refs.len(), 1000);
        }
        let mut browser = Browser {
            rows: 34,
            ..Default::default()
        };
        browser.rebuild(Some(&refs));
        let tips = browser
            .entries
            .iter()
            .map(|e| e.target.clone())
            .collect::<Vec<_>>();
        let now = crate::ui::time::now_unix();
        let mut samples = Vec::new();
        for n in 0..16 {
            let start = std::time::Instant::now();
            let values = local_activity(&repo, &tips, now);
            let score_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            browser.install_activity(values, Some(&refs));
            let sort_ms = start.elapsed().as_secs_f64() * 1000.0;
            let mut screen = crate::term::Screen::new(120, 40);
            let start = std::time::Instant::now();
            for _ in 0..200 {
                browser.move_by(1);
                if browser.selected + 1 == browser.visible.len() {
                    browser.move_by(-(browser.visible.len() as isize));
                }
                crate::ui::branches::draw(&mut browser, &mut screen, "benchmark", now);
                std::hint::black_box(screen.test_cells());
            }
            let scroll_ms = start.elapsed().as_secs_f64() * 1000.0 / 200.0;
            if n > 0 {
                samples.push(serde_json::json!({"sample":n-1,"activityMs":score_ms,"installSortMs":sort_ms,"scrollDrawMs":scroll_ms}));
            }
        }
        println!(
            "{} branch tips; one warm-up, 15 samples: {}",
            tips.len(),
            serde_json::to_string(&samples).unwrap()
        );
    }

    #[test]
    fn deleted_remote_branches_and_selection() {
        let remote = remote::Remote::parse("origin", "git@github.com:git/git.git", None).unwrap();
        let mut browser = Browser::default();
        let update = |names: &[&str]| remote::Update {
            remote: remote.clone(),
            branches: names
                .iter()
                .map(|n| remote::Branch {
                    name: (*n).into(),
                    sha: "a".repeat(40),
                })
                .collect(),
            scoped: false,
            cached: false,
            stale: false,
        };
        browser.update(update(&["a", "b"]), None);
        browser.selected = 1;
        browser.update(update(&["0", "a", "b"]), None);
        assert_eq!(browser.chosen().unwrap().target.name, "b");
        browser.update(update(&[]), None);
        assert!(browser.entries.is_empty());
    }
}
