//! A persistent, on-demand composer. No provider work happens during rendering.
use super::*;
use crate::remote::{
    self, Provider, Remote,
    requests::{Client, Created, Draft, Member},
};
use gpui_component::{
    IndexPath,
    select::{Select, SelectDelegate, SelectEvent, SelectItem, SelectState},
    text::TextView,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::Hash,
    sync::Arc,
};

#[derive(Clone)]
pub(super) struct Person {
    member: Member,
    title: SharedString,
    search: String,
}
impl Person {
    fn new(member: Member) -> Self {
        let title = format!("{} (@{})", member.name, member.username);
        Self {
            search: title.to_lowercase(),
            title: title.into(),
            member,
        }
    }
}
impl SelectItem for Person {
    type Value = Member;
    fn title(&self) -> SharedString {
        self.title.clone()
    }
    fn value(&self) -> &Member {
        &self.member
    }
    fn render(&self, _: &mut Window, _: &mut gpui::App) -> impl IntoElement {
        row().child(avatar(None, &self.member.name, 28.)).child(
            column()
                .child(self.member.name.clone())
                .child(caption(format!("@{}", self.member.username))),
        )
    }
}

// Search allocates one lowercase query and reuses a vector of indexes. Roster
// strings and user records are neither lowercased nor cloned per keystroke.
#[derive(Default)]
pub(super) struct People {
    items: Arc<Vec<Person>>,
    matches: Vec<usize>,
}
impl People {
    fn new(items: Arc<Vec<Person>>) -> Self {
        Self {
            matches: (0..items.len()).collect(),
            items,
        }
    }
    fn filter(&mut self, query: &str) {
        let query = query.to_lowercase();
        self.matches.clear();
        self.matches.extend(
            self.items
                .iter()
                .enumerate()
                .filter_map(|(i, p)| p.search.contains(&query).then_some(i)),
        );
    }
}
impl SelectDelegate for People {
    type Item = Person;
    fn items_count(&self, _: usize) -> usize {
        self.matches.len()
    }
    fn item(&self, ix: IndexPath) -> Option<&Person> {
        self.matches.get(ix.row).map(|i| &self.items[*i])
    }
    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SelectItem<Value = V>,
        V: PartialEq,
    {
        self.matches
            .iter()
            .position(|i| self.items[*i].value() == value)
            .map(|i| IndexPath::default().row(i))
    }
    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<SelectState<Self>>,
    ) -> Task<()> {
        self.filter(query);
        Task::ready(())
    }
}

pub(super) struct Composer {
    path: String,
    pub active: bool,
    loading: bool,
    sending: bool,
    uncertain: bool,
    remote: Option<Remote>,
    remotes: Vec<Remote>,
    title: Entity<InputState>,
    source: Entity<InputState>,
    target: Entity<InputState>,
    body: Entity<InputState>,
    assignees: Entity<SelectState<People>>,
    reviewers: Entity<SelectState<People>>,
    assigned: Vec<Member>,
    reviewing: Vec<Member>,
    draft: bool,
    preview: bool,
    error: String,
    warning: String,
    created: Option<Created>,
    generation: u64,
    member_count: usize,
    web_url: Option<String>,
    _subscriptions: Vec<Subscription>,
}
impl Composer {
    pub fn busy(&self) -> bool {
        self.sending
    }
    fn new(path: String, source: String, window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let title =
            cx.new(|cx| InputState::new(window, cx).placeholder("What does this change do?"));
        let source_input = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("Source branch (push it first)");
            s.set_value(source, window, cx);
            s
        });
        let target = cx.new(|cx| InputState::new(window, cx).placeholder("Target branch"));
        let body = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("markdown")
                .line_number(false)
                // gpui-component 0.5.1 applies the full placeholder run to each
                // line of an empty editor. Multiple lines panic in DirectWrite.
                .placeholder("Describe your changes in Markdown…")
        });
        let assignees =
            cx.new(|cx| SelectState::new(People::default(), None, window, cx).searchable(true));
        let reviewers =
            cx.new(|cx| SelectState::new(People::default(), None, window, cx).searchable(true));
        let mut subscriptions = Vec::new();
        for (input, reviewer) in [(&assignees, false), (&reviewers, true)] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |v, input, event, window, cx| {
                    if let SelectEvent::Confirm(Some(member)) = event
                        && let Some(c) = &mut v.composer
                    {
                        let people = if reviewer {
                            &mut c.reviewing
                        } else {
                            &mut c.assigned
                        };
                        if !c.sending
                            && c.created.is_none()
                            && !people.iter().any(|p| p.id == member.id)
                            && people.len() < 10
                        {
                            people.push(member.clone());
                        }
                        input.update(cx, |s, cx| s.set_selected_index(None, window, cx));
                        cx.notify();
                    }
                },
            ));
        }
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            path,
            active: true,
            loading: true,
            sending: false,
            uncertain: false,
            remote: None,
            remotes: Vec::new(),
            title,
            source: source_input,
            target,
            body,
            assignees,
            reviewers,
            assigned: Vec::new(),
            reviewing: Vec::new(),
            draft: false,
            preview: false,
            error: String::new(),
            warning: String::new(),
            created: None,
            generation: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) << 32,
            member_count: 0,
            web_url: None,
            _subscriptions: subscriptions,
        }
    }
}

// FIFO eviction bounds memory without dropping every warm entry at a threshold.
// Reads never mutate recency or allocate on the render path.
pub(super) struct Retained<K, V, const N: usize> {
    entries: HashMap<K, V>,
    order: VecDeque<K>,
}
impl<K, V, const N: usize> Default for Retained<K, V, N> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}
impl<K: Eq + Hash + Clone, V, const N: usize> Retained<K, V, N> {
    fn insert(&mut self, key: K, value: V) {
        if !self.entries.contains_key(&key) {
            if self.entries.len() == N
                && let Some(old) = self.order.pop_front()
            {
                self.entries.remove(&old);
            }
            self.order.push_back(key.clone());
        }
        self.entries.insert(key, value);
    }
    fn remove(&mut self, key: &K) {
        if self.entries.remove(key).is_some() {
            self.order.retain(|old| old != key);
        }
    }
}
impl<K, V, const N: usize> std::ops::Deref for Retained<K, V, N> {
    type Target = HashMap<K, V>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}
impl<K: Eq + Hash + Clone, V, const N: usize> Extend<(K, V)> for Retained<K, V, N> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, entries: T) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }
}

#[derive(Default)]
pub(super) struct Avatars {
    path: std::path::PathBuf,
    pub images: Retained<gix::ObjectId, Arc<Image>, 512>,
    pub scenario_image: Option<Arc<Image>>,
    pub reuse_scenario_image: Option<Arc<Image>>,
    attempted: Retained<gix::ObjectId, (), 4096>,
    pending: bool,
    selected: Option<(gix::ObjectId, Instant)>,
    visible: Vec<gix::ObjectId>,
    viewport_changed: Option<Instant>,
    profiles: Retained<String, Arc<Image>, 256>,
    authors: Retained<gix::ObjectId, Option<String>, 32768>,
    disk_misses: Retained<String, (), 4096>,
    authors_pending: bool,
    session: Arc<()>,
}
impl Avatars {
    pub(super) fn release_images(&mut self, cx: &mut gpui::App) {
        let mut released = HashSet::new();
        for image in self.images.values().chain(self.profiles.values()) {
            if released.insert(image.id()) {
                image.clone().remove_asset(cx);
            }
        }
        *self = Default::default(); // invalidates all in-flight UI deliveries too
    }
    fn remember_authors(
        &mut self,
        authors: impl IntoIterator<Item = (gix::ObjectId, Option<String>)>,
    ) {
        for (id, email) in authors {
            // A local lookup started before a remote preview may return a miss
            // after that preview has supplied the immutable author header.
            if email.is_some() || self.authors.get(&id).is_none_or(Option::is_none) {
                self.authors.insert(id, email);
            }
        }
    }

    fn remember_remote_authors(
        &mut self,
        selected: Option<gix::ObjectId>,
        ghosts: &HashMap<gix::ObjectId, crate::git::CommitDetails>,
    ) {
        for id in selected.into_iter().chain(self.visible.iter().copied()) {
            if let Some(details) = ghosts.get(&id)
                && !details.author_email.is_empty()
                && self.authors.get(&id).and_then(|email| email.as_deref())
                    != Some(details.author_email.as_str())
            {
                self.authors.insert(id, Some(details.author_email.clone()));
            }
        }
    }

    pub fn first_visible_ready(&self) -> bool {
        self.visible
            .first()
            .is_some_and(|id| self.image(id).is_some())
    }

    pub fn first_visible_decoded(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        self.visible
            .first()
            .and_then(|id| self.image(id))
            .is_some_and(|image| image.clone().get_render_image(window, cx).is_some())
    }

    pub fn block_network_for_scenario(&mut self, blocked: bool) {
        self.pending = blocked;
    }

    pub fn observe(&mut self, ids: impl Iterator<Item = gix::ObjectId> + Clone) -> bool {
        if !self.visible.iter().copied().eq(ids.clone()) {
            self.visible.clear();
            self.visible.extend(ids);
            self.viewport_changed = Some(Instant::now());
            return true;
        }
        false
    }

    pub fn image(&self, id: &gix::ObjectId) -> Option<&Arc<Image>> {
        self.images.get(id).or_else(|| {
            self.authors
                .get(id)
                .and_then(|email| email.as_ref())
                .and_then(|email| self.profiles.get(email))
        })
    }

    pub fn summary(&self) -> String {
        format!(
            "avatar_authors={} avatar_profiles={} avatar_visible_ready={}",
            self.authors.len(),
            self.profiles.len(),
            self.visible
                .iter()
                .filter(|id| self.image(id).is_some())
                .count()
        )
    }

    pub fn seed_reuse_scenario(&mut self) {
        if let Some(image) = &self.reuse_scenario_image {
            for email in self.authors.values().flatten() {
                if !self.profiles.contains_key(email) {
                    self.profiles.insert(email.clone(), image.clone());
                }
            }
        }
    }
}

fn local_authors(
    path: &std::path::Path,
    ids: &[gix::ObjectId],
) -> Vec<(gix::ObjectId, Option<String>)> {
    use gix::{bstr::ByteSlice, prelude::FindExt};
    let repo = crate::git::open(path).ok();
    let mut buffer = Vec::new();
    ids.iter()
        .map(|id| {
            let email = repo.as_ref().and_then(|r| {
                let tokens = r.objects.find_commit_iter(id, &mut buffer).ok()?;
                for token in tokens {
                    if let gix::objs::commit::ref_iter::Token::Author { signature } = token.ok()? {
                        return (!signature.email.is_empty())
                            .then(|| signature.email.to_str_lossy().into_owned());
                    }
                }
                None
            });
            (*id, email)
        })
        .collect()
}

// This path cannot initialize credentials, an HTTP client, or make a request.
// Resolve all visible cached profiles before considering any network misses.
fn cached_profiles(
    cache: &remote::requests::PhotoCache,
    remote: &Remote,
    emails: impl Iterator<Item = String>,
) -> Vec<(String, Option<Arc<Image>>)> {
    let mut seen = HashSet::new();
    let mut images = HashMap::new();
    emails
        .filter_map(|email| {
            if !seen.insert(email.clone()) {
                return None;
            }
            let image = cache.identity(remote, &email).and_then(|url| {
                images
                    .entry(url.clone())
                    .or_insert_with(|| {
                        let bytes = cache.image(&url)?;
                        let format = remote::requests::image_format(&bytes)?;
                        Some(Arc::new(Image::from_bytes(format, bytes)))
                    })
                    .clone()
            });
            Some((email, image))
        })
        .collect()
}
pub(super) fn avatar(image: Option<&Arc<Image>>, name: &str, size: f32) -> Div {
    let frame = div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .bg(rgb(CONTROL));
    if let Some(image) = image {
        frame.child(
            img(image.clone())
                .size_full()
                .rounded_full()
                .object_fit(ObjectFit::Cover),
        )
    } else {
        frame
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(ACCENT_HOVER))
            .text_size(px(size * 0.43))
            .child(
                name.chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string(),
            )
    }
}

impl Desktop {
    pub(super) fn request_scenario(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.engine.mark("request composer requested");
        let mut composer = Composer::new(
            self.engine.repository_path(),
            "feature/review".into(),
            window,
            cx,
        );
        composer.loading = false;
        composer.member_count = 10_000;
        composer.remote = Remote::parse("origin", "https://github.com/example/project.git", None);
        composer.title.update(cx, |s, cx| {
            s.set_value("Make remote reviews feel native", window, cx)
        });
        composer
            .target
            .update(cx, |s, cx| s.set_value("main", window, cx));
        composer.body.update(cx, |s, cx| s.set_value("## Summary\nA **Markdown** description with `code`, a [link](https://example.com), and a task list.\n\n## Testing\n- [x] Provider fixtures\n- [ ] Review", window, cx));
        let people = Arc::new(
            (0..10_000)
                .map(|id| {
                    Person::new(Member {
                        id,
                        username: format!("member-{id}"),
                        name: format!("Project Member {id}"),
                        avatar: None,
                    })
                })
                .collect(),
        );
        composer.assignees.update(cx, |s, cx| {
            s.set_items(People::new(Arc::clone(&people)), window, cx)
        });
        composer
            .reviewers
            .update(cx, |s, cx| s.set_items(People::new(people), window, cx));
        self.composer = Some(composer);
        self.engine.mark("request composer ready");
        cx.notify();
    }

    pub(super) fn request_scenario_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scenario.as_deref() == Some("request-clear") {
            if let Some(c) = &mut self.composer {
                c.preview = false;
                c.body.update(cx, |s, cx| {
                    s.set_value(
                        if self.scroll_step.is_multiple_of(2) {
                            "## Summary\nA **Markdown** description.\n\n- [ ] Test"
                        } else {
                            ""
                        },
                        window,
                        cx,
                    )
                });
            }
            cx.notify();
        } else if self.scenario.as_deref() == Some("request-tabs") {
            self.action(
                Action::Tab(if self.scroll_step.is_multiple_of(2) {
                    Tab::Request
                } else {
                    Tab::Graph
                }),
                window,
                cx,
            );
        } else if let Some(c) = &mut self.composer {
            c.preview = self.scroll_step.is_multiple_of(2);
            if !c.preview {
                c.title.update(cx, |s, cx| {
                    s.set_value(format!("Review change {}", self.scroll_step), window, cx)
                });
            }
            cx.notify();
        }
    }

    pub(super) fn request_summary(&self, cx: &gpui::App) -> String {
        self.composer
            .as_ref()
            .map(|c| {
                format!(
                    "request_open=true request_members={} request_sending={} request_body_bytes={}",
                    c.member_count,
                    c.sending,
                    c.body.read(cx).value().len()
                )
            })
            .unwrap_or_else(|| "request_open=false".into())
    }
    pub(super) fn sync_request_repository(&mut self) {
        if self
            .composer
            .as_ref()
            .is_some_and(|c| c.path != self.engine.repository_path())
        {
            self.composer = None;
        }
    }
    pub(super) fn open_request(
        &mut self,
        source: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(c) = &mut self.composer {
            c.active = true;
            cx.notify();
            return;
        }
        self.engine.mark("request composer requested");
        self.composer = Some(Composer::new(
            self.engine.repository_path(),
            source,
            window,
            cx,
        ));
        self.load_request(None, false, window, cx);
        cx.notify();
    }

    fn load_request(
        &mut self,
        chosen: Option<Remote>,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(c) = &mut self.composer else {
            return;
        };
        c.loading = true;
        c.error.clear();
        c.warning.clear();
        c.generation += 1;
        let generation = c.generation;
        let path = c.path.clone();
        let no_cache = self.engine.no_cache();
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let path = std::path::Path::new(&path);
                    let remotes = remote::configured(path)?.0;
                    let remote = chosen
                        .or_else(|| {
                            remotes
                                .iter()
                                .find(|r| r.name == "origin")
                                .or_else(|| remotes.first())
                                .cloned()
                        })
                        .ok_or("Configure a GitHub or GitLab remote in Settings first.")?;
                    let metadata = Client::new(path, &remote, no_cache)
                        .and_then(|mut client| client.metadata(force));
                    Ok::<_, String>((remotes, remote, metadata))
                })
                .await;
            let _ = view.update_in(cx, |v, window, cx| {
                let Some(c) = &mut v.composer else {
                    return;
                };
                if c.generation != generation {
                    return;
                }
                c.loading = false;
                match result {
                    Ok((remotes, remote, metadata)) => {
                        c.remotes = remotes;
                        c.remote = Some(remote);
                        match metadata {
                            Ok(data) => {
                                c.member_count = data.members.len();
                                c.web_url = data.web_url;
                                if c.target.read(cx).value().is_empty() {
                                    c.target.update(cx, |s, cx| {
                                        s.set_value(data.default_branch, window, cx)
                                    });
                                }
                                c.warning = data.warning;
                                let people =
                                    Arc::new(data.members.into_iter().map(Person::new).collect());
                                c.assignees.update(cx, |s, cx| {
                                    s.set_items(People::new(Arc::clone(&people)), window, cx)
                                });
                                c.reviewers.update(cx, |s, cx| {
                                    s.set_items(People::new(people), window, cx)
                                });
                            }
                            Err(error) => c.error = error,
                        }
                    }
                    Err(error) => c.error = error,
                }
                v.engine.mark("request composer ready");
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_request(&mut self, cx: &mut Context<Self>) {
        let Some(c) = &mut self.composer else {
            return;
        };
        if c.sending || c.loading || c.created.is_some() || c.uncertain {
            return;
        }
        let Some(remote) = c.remote.clone() else {
            return;
        };
        let draft = Draft {
            source: c.source.read(cx).value().trim().into(),
            target: c.target.read(cx).value().trim().into(),
            title: c.title.read(cx).value().to_string(),
            description: c.body.read(cx).value().to_string(),
            assignees: c.assigned.clone(),
            reviewers: c.reviewing.clone(),
            draft: c.draft,
        };
        if let Err(error) = draft.validate() {
            c.error = error;
            cx.notify();
            return;
        }
        c.sending = true;
        c.error.clear();
        let path = c.path.clone();
        let generation = c.generation;
        let no_cache = self.engine.no_cache();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    Client::new(std::path::Path::new(&path), &remote, no_cache)?.create(&draft)
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                let Some(c) = &mut v.composer else {
                    return;
                };
                if c.generation != generation {
                    return;
                }
                c.sending = false;
                match result {
                    Ok(created) => c.created = Some(created),
                    Err(error) => {
                        c.uncertain = error.contains("unknown")
                            || error.contains("may have succeeded")
                            || error.starts_with("Created,");
                        c.error = error;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn reset_avatar_repository(&mut self) {
        let path = self
            .engine
            .repository_root()
            .unwrap_or_else(|| std::path::Path::new(""));
        if self.avatars.path != path {
            self.avatars = Avatars {
                path: path.to_path_buf(),
                ..Avatars::default()
            };
        }
    }

    // Reuse already loaded remote headers; local headers and disk photos are read
    // on a worker independently of the network debounce and provider requests.
    pub(super) fn resolve_avatar_authors(&mut self, cx: &mut Context<Self>) {
        if self.engine.app.settings.open || self.image_cache.busy {
            return;
        }
        self.avatars
            .remember_remote_authors(self.engine.app.selected_id(), &self.engine.app.ghosts);
        if self.scenario.as_deref() == Some("avatar-scroll")
            || self.avatars.authors_pending
            || (self.engine.app.branches.remotes.is_empty()
                && self.scenario.as_deref() != Some("avatar-reuse-scroll"))
        {
            return;
        }
        let remote = self.avatar_remote();
        let read_cache = remote.is_some()
            && !self.engine.no_cache()
            && self.scenario.as_deref() != Some("avatar-reuse-scroll");
        let selected = self.engine.app.selected_id();
        let ids: Vec<_> = selected
            .into_iter()
            .chain(
                self.avatars
                    .visible
                    .iter()
                    .copied()
                    .filter(|id| Some(*id) != selected),
            )
            .filter(|id| match self.avatars.authors.get(id) {
                None => true,
                Some(Some(email)) => {
                    read_cache
                        && !self.avatars.profiles.contains_key(email)
                        && !self.avatars.disk_misses.contains_key(email)
                }
                Some(None) => false,
            })
            .take(80)
            .collect();
        if ids.is_empty() {
            return;
        }
        self.avatars.authors_pending = true;
        let path = self.avatars.path.clone();
        let session = self.avatars.session.clone();
        let known: Vec<_> = ids
            .iter()
            .filter_map(|id| {
                self.avatars
                    .authors
                    .get(id)
                    .map(|email| (*id, email.clone()))
            })
            .collect();
        let available: HashSet<_> = self.avatars.profiles.keys().cloned().collect();
        let start = matches!(
            self.scenario.as_deref(),
            Some("avatar-reuse-scroll" | "avatar-cache" | "avatar-cache-busy")
        )
        .then(Instant::now);
        cx.spawn(async move |view, cx| {
            let (authors, profiles) = cx
                .background_executor()
                .spawn(async move {
                    let unknown: Vec<_> = ids
                        .into_iter()
                        .filter(|id| !known.iter().any(|(known, _)| known == id))
                        .collect();
                    let mut authors = known;
                    if !unknown.is_empty() {
                        authors.extend(local_authors(&path, &unknown));
                    }
                    let emails: Vec<_> = authors
                        .iter()
                        .filter_map(|(_, e)| e.as_ref())
                        .filter(|email| !available.contains(*email))
                        .cloned()
                        .collect();
                    let profiles =
                        if let Some(remote) = remote.filter(|_| read_cache && !emails.is_empty()) {
                            let cache = crate::git::open(&path).ok().and_then(|repo| {
                                remote::requests::PhotoCache::new(&crate::git::git_dir(&repo))
                            });
                            match cache {
                                Some(cache) => cached_profiles(&cache, &remote, emails.into_iter()),
                                None => emails.into_iter().map(|email| (email, None)).collect(),
                            }
                        } else {
                            Vec::new()
                        };
                    (authors, profiles)
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                if !Arc::ptr_eq(&v.avatars.session, &session) {
                    return;
                }
                v.avatars.authors_pending = false;
                // A photo request may finish before its local author header does.
                for (id, email) in &authors {
                    if let (Some(email), Some(image)) = (email, v.avatars.images.get(id)) {
                        v.avatars.profiles.insert(email.clone(), image.clone());
                    }
                }
                v.avatars.remember_authors(authors);
                for (email, image) in profiles {
                    if let Some(image) = image {
                        v.avatars.profiles.insert(email, image);
                    } else if !v.avatars.profiles.contains_key(&email) {
                        v.avatars.disk_misses.insert(email, ());
                    }
                }
                if let Some(start) = start {
                    v.avatars.seed_reuse_scenario();
                    v.engine.mark(format!(
                        "avatar local resolution {:.3} ms",
                        start.elapsed().as_secs_f64() * 1000.
                    ));
                    if matches!(
                        v.scenario.as_deref(),
                        Some("avatar-cache" | "avatar-cache-busy")
                    ) {
                        v.engine.mark(format!(
                            "avatar cache state {} first_remote={}",
                            v.avatars.summary(),
                            v.avatars.visible.first().is_some_and(|id| v
                                .engine
                                .app
                                .ghosts
                                .contains_key(id))
                        ));
                    }
                }
                // The viewport may have moved while this worker was reading.
                v.resolve_avatar_authors(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn sync_avatars(&mut self, cx: &mut Context<Self>) {
        if self.engine.app.settings.open || self.image_cache.busy {
            return;
        }
        if self.scenario.as_deref() == Some("avatar-scroll") {
            return;
        }
        self.reset_avatar_repository();
        self.resolve_avatar_authors(cx);
        if self.scenario.as_deref() == Some("avatar-reuse-scroll") {
            return;
        }
        let path = self.avatars.path.clone();
        let selected = self
            .engine
            .app
            .details
            .as_ref()
            .filter(|d| Some(d.id) == self.engine.app.selected_id())
            .map(|d| d.id);
        if self.avatars.selected.map(|v| v.0) != selected {
            self.avatars.selected = selected.map(|id| (id, Instant::now()));
        }
        let Some((id, since)) = self.avatars.selected.or_else(|| {
            self.avatars.visible.first().copied().map(|id| {
                (
                    id,
                    self.avatars.viewport_changed.unwrap_or_else(Instant::now),
                )
            })
        }) else {
            return;
        };
        if since.elapsed() < Duration::from_millis(350)
            || self.avatars.pending
            || self
                .avatars
                .viewport_changed
                .is_some_and(|s| s.elapsed() < Duration::from_millis(350))
        {
            return;
        }
        // Discovery is already done on a worker. Never read config or Git here.
        let Some(remote) = self.avatar_remote() else {
            return;
        };
        let ids: Vec<_> = std::iter::once(id)
            .chain(
                self.avatars
                    .visible
                    .iter()
                    .copied()
                    .filter(|other| *other != id),
            )
            .filter(|id| {
                !self.avatars.attempted.contains_key(id) && self.avatars.image(id).is_none()
            })
            .take(4)
            .map(|id| (id, self.avatars.authors.get(&id).cloned().flatten()))
            .collect();
        if ids.is_empty() {
            return;
        }
        self.avatars.pending = true;
        let session = self.avatars.session.clone();
        let request_path = path.clone();
        let no_cache = self.engine.no_cache();
        let photos = remote::requests::PhotoCache::new(&path);
        cx.spawn(async move |view, cx| {
            let images = cx
                .background_executor()
                .spawn(async move {
                    let mut images = Vec::new();
                    let Ok(mut client) = Client::new(&path, &remote, no_cache) else {
                        return images;
                    };
                    client.set_photo_cache(photos);
                    let mut resolved = HashMap::new();
                    for (id, email) in ids {
                        let cached = email.as_ref().and_then(|e| resolved.get(e)).cloned();
                        let image = cached.or_else(|| {
                            let image = client
                                .author_for_email(&id.to_string(), email.as_deref().unwrap_or(""))
                                .and_then(|bytes| {
                                    remote::requests::image_format(&bytes)
                                        .map(|format| Arc::new(Image::from_bytes(format, bytes)))
                                });
                            if let (Some(email), Some(image)) = (&email, &image) {
                                resolved.insert(email.clone(), image.clone());
                            }
                            image
                        });
                        images.push((id, email, image));
                    }
                    images
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                if v.avatars.path != request_path
                    || v.engine.repository_root() != Some(request_path.as_path())
                    || !Arc::ptr_eq(&v.avatars.session, &session)
                {
                    return;
                }
                v.avatars.pending = false;
                for (id, email, image) in images {
                    v.avatars.attempted.insert(id, ());
                    if let Some(image) = image {
                        // Failed/unpublished commits never poison an author's profile.
                        if let Some(email) = email {
                            v.avatars.disk_misses.remove(&email);
                            v.avatars.profiles.insert(email, image.clone());
                        }
                        v.avatars.images.insert(id, image);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn avatar_remote(&self) -> Option<Remote> {
        self.engine
            .app
            .branches
            .remotes
            .values()
            .find(|u| u.remote.name == "origin")
            .or_else(|| self.engine.app.branches.remotes.values().next())
            .map(|u| u.remote.clone())
    }

    fn people_field(&self, reviewer: bool, cx: &mut Context<Self>) -> Div {
        let c = self.composer.as_ref().unwrap();
        let people = if reviewer { &c.reviewing } else { &c.assigned };
        let locked = c.sending || c.created.is_some() || c.uncertain;
        column()
            .gap_2()
            .child(caption(if reviewer { "Reviewers" } else { "Assignees" }))
            .child(
                Select::new(if reviewer { &c.reviewers } else { &c.assignees })
                    .placeholder("Add project member…")
                    .search_placeholder("Search name or username…")
                    .disabled(locked || c.loading || people.len() >= 10)
                    .menu_width(px(320.)),
            )
            .children(people.iter().enumerate().map(|(i, p)| {
                row()
                    .rounded_md()
                    .bg(rgb(CONTROL))
                    .px_2()
                    .py_1()
                    .child(avatar(None, &p.name, 24.))
                    .child(div().flex_1().truncate().child(format!("@{}", p.username)))
                    .child(
                        Button::new((
                            if reviewer {
                                "remove-reviewer"
                            } else {
                                "remove-assignee"
                            },
                            i,
                        ))
                        .ghost()
                        .xsmall()
                        .label("×")
                        .tooltip("Remove member")
                        .disabled(locked)
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(c) = &mut v.composer {
                                let people = if reviewer {
                                    &mut c.reviewing
                                } else {
                                    &mut c.assigned
                                };
                                if i < people.len() {
                                    people.remove(i);
                                }
                                cx.notify();
                            }
                        })),
                    )
            }))
    }

    pub(super) fn request_content(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = self.composer.as_ref().unwrap();
        let label = if c
            .remote
            .as_ref()
            .is_some_and(|r| r.provider == Provider::Gitlab)
        {
            "merge request"
        } else {
            "pull request"
        };
        let locked = c.sending || c.created.is_some() || c.uncertain;
        let weak = cx.entity().downgrade();
        let choices = c.remotes.clone();
        let selected = c.remote.as_ref().map(|r| r.name.clone());
        let remote_select = Button::new("request-project")
            .ghost()
            .label(
                c.remote
                    .as_ref()
                    .map(|r| format!("{} · {} ▾", r.name, r.project))
                    .unwrap_or_else(|| "Choose project ▾".into()),
            )
            .disabled(locked || c.loading)
            .dropdown_menu(move |mut menu, _, _| {
                for remote in &choices {
                    let weak = weak.clone();
                    let remote = remote.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{} · {}", remote.name, remote.project))
                            .checked(selected.as_ref() == Some(&remote.name))
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |v, cx| {
                                    if let Some(c) = &mut v.composer {
                                        if c.remote.as_ref() == Some(&remote) {
                                            return;
                                        }
                                        c.assigned.clear();
                                        c.reviewing.clear();
                                        c.target.update(cx, |s, cx| s.set_value("", window, cx));
                                    }
                                    v.load_request(Some(remote.clone()), false, window, cx);
                                });
                            }),
                    );
                }
                menu
            });
        let mut editor = column()
            .flex_1()
            .gap_3()
            .child(caption("Title"))
            .child(Input::new(&c.title).disabled(locked))
            .child(
                row().justify_between().child(caption("Description")).child(
                    row()
                        .gap_1()
                        .children([(false, "Write"), (true, "Preview")].into_iter().map(
                            |(preview, title)| {
                                Button::new(if preview {
                                    "request-preview"
                                } else {
                                    "request-write"
                                })
                                .small()
                                .ghost()
                                .selected(c.preview == preview)
                                .label(title)
                                .on_click(cx.listener(
                                    move |v, _, _, cx| {
                                        if let Some(c) = &mut v.composer {
                                            c.preview = preview;
                                            cx.notify();
                                        }
                                    },
                                ))
                            },
                        )),
                ),
            );
        editor = if c.preview {
            editor.child(
                column()
                    .id("request-markdown-preview")
                    .h(px(300.))
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(PANEL))
                    .child(
                        TextView::markdown("request-markdown", c.body.read(cx).value(), window, cx)
                            .w_full()
                            .h(px(266.))
                            .scrollable(true)
                            .selectable(true),
                    ),
            )
        } else {
            editor.child(Input::new(&c.body).h(px(300.)).disabled(locked))
        };
        let form = column().gap_5().w_full().max_w(px(1080.))
            .child(row().justify_between()
                .child(column().gap_1().child(div().text_size(px(26.)).child(format!("New {label}")))
                    .child(caption("Share your changes and bring your team into the review.")))
                .child(Button::new("close-request").ghost().label("Close tab").disabled(c.sending)
                    .on_click(cx.listener(|v, _, _, cx| { v.composer = None; cx.notify(); }))))
            .child(row().child(icon("branch").text_color(rgb(ACCENT))).child(remote_select))
            .child(row().items_start().gap_4()
                .child(column().flex_1().gap_2().child(caption("Source branch")).child(Input::new(&c.source).disabled(locked)))
                .child(div().pt_6().text_color(rgb(ACCENT)).child("→"))
                .child(column().flex_1().gap_2().child(caption("Target branch")).child(Input::new(&c.target).disabled(locked))))
            .child(caption("The source branch must be pushed to the selected project. Your draft stays here when switching tabs."))
            .child(div().h(px(1.)).bg(rgb(LINE)))
            .child(row().items_start().gap_6().child(editor).child(column().w(px(260.)).flex_shrink_0().gap_5()
                .child(self.people_field(false, cx)).child(self.people_field(true, cx))
                .child(caption("Suggestions include project members. Search by name or @username. Up to 10 per field."))))
            .child(caption("Markdown supported · headings, lists, links, code blocks and task lists"))
            .when(c.loading, |s| s.child(caption("Loading project and member suggestions… You can start writing.")))
            .when(!c.loading && !locked, |s| s.child(Button::new("retry-members").ghost().small().label("Refresh project members")
                .on_click(cx.listener(|v, _, window, cx| { let remote = v.composer.as_ref().and_then(|c| c.remote.clone()); v.load_request(remote, true, window, cx); }))))
            ;
        let actions = row()
            .w_full()
            .max_w(px(1080.))
            .justify_between()
            .py_3()
            .child(
                Checkbox::new("request-draft")
                    .label("Create as draft")
                    .checked(c.draft)
                    .disabled(locked)
                    .on_click(cx.listener(|v, checked, _, cx| {
                        if let Some(c) = &mut v.composer {
                            c.draft = *checked;
                            cx.notify();
                        }
                    })),
            )
            .child(
                Button::new("submit-request")
                    .primary()
                    .label(if c.sending {
                        "Creating…".into()
                    } else {
                        format!("Create {label}")
                    })
                    .disabled(locked || c.loading || c.remote.is_none())
                    .on_click(cx.listener(|v, _, _, cx| v.submit_request(cx))),
            );
        column()
            .flex_1()
            .child(
                column()
                    .id("request-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_6()
                    .items_center()
                    .child(form),
            )
            .child(
                column()
                    .flex_shrink_0()
                    .px_6()
                    .items_center()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(PANEL))
                    .when(!c.error.is_empty(), |s| {
                        s.child(
                            div()
                                .w_full()
                                .max_w(px(1080.))
                                .pt_3()
                                .text_color(rgb(REMOVED))
                                .child(c.error.clone()),
                        )
                    })
                    .when(!c.warning.is_empty(), |s| {
                        s.child(caption(c.warning.clone()).w_full().max_w(px(1080.)).pt_2())
                    })
                    .when(c.uncertain, |s| {
                        s.when_some(c.web_url.clone(), |s, url| {
                            s.child(
                                Button::new("check-request-project")
                                    .ghost()
                                    .label("Check project in browser")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                        })
                    })
                    .when_some(c.created.as_ref(), |s, created| {
                        let url = created.url.clone();
                        s.child(
                            row()
                                .w_full()
                                .max_w(px(1080.))
                                .pt_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(rgb(ADDED))
                                        .child(format!("Created {label} #{}", created.number)),
                                )
                                .child(
                                    Button::new("open-created-request")
                                        .label("Open in browser")
                                        .on_click(move |_, _, cx| cx.open_url(&url)),
                                ),
                        )
                        .when(!created.warning.is_empty(), |s| {
                            s.child(caption(created.warning.clone()).w_full().max_w(px(1080.)))
                        })
                    })
                    .child(actions),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Arc, Instant, Member, People, Person};
    use gix::bstr::ByteSlice;

    #[test]
    fn remote_author_metadata_reuses_a_profile_and_survives_a_late_local_miss() {
        let id = gix::ObjectId::from_bytes_or_panic(&[1; 20]);
        let image = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            b"\x89PNG\r\n\x1a\nfixture".to_vec(),
        ));
        let details = crate::git::CommitDetails {
            id,
            parents: Vec::new(),
            author_name: "Shared Name".into(),
            author_email: "alice@example.com".into(),
            author_time: 0,
            author_offset: 0,
            committer_name: String::new(),
            committer_email: String::new(),
            commit_time: 0,
            commit_offset: 0,
            message: String::new(),
        };
        let mut avatars = super::Avatars {
            pending: true,
            ..Default::default()
        };
        avatars.observe(std::iter::once(id));
        avatars.authors.insert(id, None); // object is absent from the local clone
        avatars
            .profiles
            .insert(details.author_email.clone(), image.clone());
        avatars.remember_remote_authors(None, &[(id, details)].into_iter().collect());
        assert!(Arc::ptr_eq(avatars.image(&id).unwrap(), &image));
        avatars.remember_authors([(id, None)]); // already running local worker finishes
        assert!(Arc::ptr_eq(avatars.image(&id).unwrap(), &image));
        assert!(avatars.pending); // no network request was needed
    }

    #[test]
    fn retained_cache_evicts_one_entry_and_updates_do_not_duplicate_its_queue() {
        let mut cache = super::Retained::<u32, u32, 3>::default();
        for n in 0..3 {
            cache.insert(n, n);
        }
        cache.insert(1, 10);
        cache.insert(3, 3);
        assert!(!cache.contains_key(&0));
        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.order.len(), 3);
        cache.remove(&2);
        cache.insert(2, 20);
        cache.insert(4, 4);
        assert!(!cache.contains_key(&1));
        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.order.len(), 3);
    }

    #[test]
    fn disk_profiles_resolve_past_a_miss_and_share_image_allocations_offline() {
        let dir = tempfile::tempdir().unwrap();
        let cache = crate::remote::requests::PhotoCache::at(dir.path().into());
        let remote =
            crate::remote::Remote::parse("origin", "https://github.com/a/repo.git", None).unwrap();
        let url = "https://avatars.example/u/42?s=64";
        let bytes = b"\x89PNG\r\n\x1a\nfixture";
        assert!(cache.store_image(url, bytes));
        for email in ["alice@example.com", "alias@example.com"] {
            cache.store_identity(&remote, email, url);
        }
        let profiles = super::cached_profiles(
            &cache,
            &remote,
            [
                "missing@example.com",
                "alice@example.com",
                "alias@example.com",
                "alice@example.com",
            ]
            .into_iter()
            .map(String::from),
        );
        assert_eq!(profiles.len(), 3);
        assert!(profiles[0].1.is_none());
        assert_eq!(profiles[1].1.as_ref().unwrap().bytes, bytes);
        assert!(Arc::ptr_eq(
            profiles[1].1.as_ref().unwrap(),
            profiles[2].1.as_ref().unwrap()
        ));
    }

    #[test]
    fn returning_to_an_earlier_commit_reuses_the_profile_after_512_rows() {
        let image = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            b"\x89PNG\r\n\x1a\nfixture".to_vec(),
        ));
        let mut avatars = super::Avatars::default();
        avatars
            .profiles
            .insert("shared@example.com".into(), image.clone());
        let mut first = None;
        for n in 0..700_u32 {
            let mut bytes = [0; 20];
            bytes[..4].copy_from_slice(&n.to_le_bytes());
            let id = gix::ObjectId::from_bytes_or_panic(&bytes);
            first.get_or_insert(id);
            avatars
                .authors
                .insert(id, Some("shared@example.com".into()));
            avatars.images.insert(id, image.clone());
            avatars.attempted.insert(id, ());
        }
        let first = first.unwrap();
        assert!(!avatars.images.contains_key(&first)); // per-commit image was evicted
        avatars.observe(std::iter::once(first));
        assert!(Arc::ptr_eq(avatars.image(&first).unwrap(), &image));
        assert_eq!(avatars.authors.len(), 700);
        assert_eq!(avatars.images.len(), 512);
        assert_eq!(avatars.attempted.len(), 700);
    }

    #[test]
    fn known_author_image_is_reused_during_scrolling_and_an_inflight_download() {
        use crate::git::command::checked;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        checked(path, &["init", "-q"]).unwrap();
        checked(path, &["config", "user.name", "Shared Display Name"]).unwrap();
        let mut ids = Vec::new();
        for email in ["alice@example.com", "alice@example.com", "bob@example.com"] {
            checked(path, &["config", "user.email", email]).unwrap();
            checked(
                path,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    email,
                ],
            )
            .unwrap();
            ids.push(
                gix::ObjectId::from_hex(checked(path, &["rev-parse", "HEAD"]).unwrap().trim())
                    .unwrap(),
            );
        }
        let image = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            b"\x89PNG\r\n\x1a\nfixture".to_vec(),
        ));
        let mut avatars = super::Avatars {
            pending: true,
            ..Default::default()
        };
        avatars
            .profiles
            .insert("alice@example.com".into(), image.clone());
        avatars.observe(ids.iter().copied()); // viewport has just changed, not settled
        avatars.authors.extend(super::local_authors(path, &ids));
        assert!(avatars.viewport_changed.is_some());
        assert!(Arc::ptr_eq(avatars.image(&ids[0]).unwrap(), &image));
        assert!(Arc::ptr_eq(avatars.image(&ids[1]).unwrap(), &image));
        assert!(avatars.image(&ids[2]).is_none()); // same name is not the same identity
        assert!(avatars.attempted.is_empty()); // no per-commit photo request required
    }

    #[test]
    #[ignore]
    fn benchmark_local_author_reuse() {
        let path = std::env::var_os("GIT_RUSTLE_AUTHOR_REPO")
            .expect("read-only benchmark repository required");
        let path = std::path::Path::new(&path);
        let ids: Vec<_> =
            crate::git::command::checked(path, &["rev-list", "--max-count=1000", "HEAD"])
                .unwrap()
                .lines()
                .map(|s| gix::ObjectId::from_hex(s).unwrap())
                .collect();
        assert!(ids.len() >= 640);
        let image = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            b"\x89PNG\r\n\x1a\nfixture".to_vec(),
        ));
        let mut metadata = Vec::new();
        let mut lookup = Vec::new();
        for sample in 0..16 {
            let ids = &ids[sample * 40..sample * 40 + 40];
            let start = Instant::now();
            let authors = super::local_authors(path, ids);
            let metadata_ms = start.elapsed().as_secs_f64() * 1000.;
            let mut avatars = super::Avatars::default();
            for (_, email) in &authors {
                if let Some(email) = email {
                    avatars.profiles.insert(email.clone(), image.clone());
                }
            }
            avatars.authors.extend(authors);
            let start = Instant::now();
            for _ in 0..1000 {
                for id in ids {
                    assert!(std::hint::black_box(avatars.image(id)).is_some());
                }
            }
            let lookup_ms = start.elapsed().as_secs_f64() * 1000. / 40_000.;
            if sample > 0 {
                metadata.push(metadata_ms);
                lookup.push(lookup_ms);
            }
        }
        println!("local_40_authors_ms={metadata:?}\nshared_image_lookup_ms_per_row={lookup:?}");
    }

    #[test]
    fn member_search_matches_name_and_username_without_copying_records() {
        let items = Arc::new(vec![
            Person::new(Member {
                id: 1,
                username: "alice".into(),
                name: "Alice Example".into(),
                avatar: None,
            }),
            Person::new(Member {
                id: 2,
                username: "bob".into(),
                name: "Robert".into(),
                avatar: None,
            }),
        ]);
        let mut people = People::new(items.clone());
        people.filter("@BOB");
        assert_eq!(people.matches, [1]);
        people.filter("example");
        assert_eq!(people.matches, [0]);
        people.filter("");
        assert_eq!(people.matches.len(), 2);
        assert!(Arc::ptr_eq(&items, &people.items));
    }

    #[test]
    #[ignore]
    fn benchmark_member_search() {
        let items = Arc::new(
            (0..10_000)
                .map(|i| {
                    Person::new(Member {
                        id: i,
                        username: format!("member-{i}"),
                        name: format!("Project Member {i}"),
                        avatar: None,
                    })
                })
                .collect(),
        );
        let mut people = People::new(items);
        let mut samples = Vec::new();
        for sample in 0..16 {
            let start = Instant::now();
            for _ in 0..100 {
                for query in ["m", "me", "mem", "member-9", "no match", ""] {
                    people.filter(query);
                    std::hint::black_box(&people.matches);
                }
            }
            if sample > 0 {
                samples.push(start.elapsed().as_secs_f64() * 1000. / 600.);
            }
        }
        println!("member_search_ms_per_query={samples:?}");
    }
}
