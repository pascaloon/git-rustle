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
    collections::{HashMap, HashSet},
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
                .placeholder("## Summary\nDescribe your changes…\n\n## Testing\n- [ ] Tests pass")
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

#[derive(Default)]
pub(super) struct Avatars {
    path: String,
    pub images: HashMap<gix::ObjectId, Arc<Image>>,
    pub scenario_image: Option<Arc<Image>>,
    attempted: HashSet<gix::ObjectId>,
    pending: bool,
    selected: Option<(gix::ObjectId, Instant)>,
    visible: Vec<gix::ObjectId>,
    viewport_changed: Option<Instant>,
    profiles: Arc<std::sync::Mutex<HashMap<String, Option<Arc<Image>>>>>,
}
impl Avatars {
    pub fn observe(&mut self, ids: impl Iterator<Item = gix::ObjectId> + Clone) {
        if !self.visible.iter().copied().eq(ids.clone()) {
            self.visible.clear();
            self.visible.extend(ids);
            self.viewport_changed = Some(Instant::now());
        }
    }
}
pub(super) fn avatar(image: Option<&Arc<Image>>, name: &str, size: f32) -> Div {
    let frame = div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .bg(rgb(CONTROL));
    if let Some(image) = image {
        frame.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
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
        if self.scenario.as_deref() == Some("request-tabs") {
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

    pub(super) fn request_summary(&self) -> String {
        self.composer
            .as_ref()
            .map(|c| {
                format!(
                    "request_open=true request_members={} request_sending={}",
                    c.member_count, c.sending
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

    pub(super) fn sync_avatars(&mut self, cx: &mut Context<Self>) {
        if self.scenario.as_deref() == Some("avatar-scroll") {
            return;
        }
        let path = self.engine.repository_path();
        if self.avatars.path != path {
            self.avatars = Avatars {
                path: path.clone(),
                ..Avatars::default()
            };
        }
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
        let Some(remote) = self
            .engine
            .app
            .branches
            .remotes
            .values()
            .find(|u| u.remote.name == "origin")
            .or_else(|| self.engine.app.branches.remotes.values().next())
            .map(|u| u.remote.clone())
        else {
            return;
        };
        if self.avatars.attempted.len() >= 512 {
            self.avatars.attempted.clear();
            self.avatars.images.clear();
        }
        let ids: Vec<_> = std::iter::once(id)
            .chain(self.avatars.visible.iter().copied())
            .filter(|id| !self.avatars.attempted.contains(id))
            .take(40)
            .collect();
        if ids.is_empty() {
            return;
        }
        self.avatars.pending = true;
        let profiles = self.avatars.profiles.clone();
        let request_profiles = profiles.clone();
        let request_path = path.clone();
        let no_cache = self.engine.no_cache();
        cx.spawn(async move |view, cx| {
            let images = cx
                .background_executor()
                .spawn(async move {
                    let mut images = Vec::new();
                    let path = std::path::Path::new(&path);
                    let Ok(mut client) = Client::new(path, &remote, no_cache) else {
                        return images;
                    };
                    let repo = crate::git::open(path).ok();
                    let mut misses = 0;
                    for id in ids {
                        let email = repo
                            .as_ref()
                            .and_then(|r| crate::git::commit_details(r, &id).ok())
                            .map(|d| d.author_email)
                            .filter(|e| !e.is_empty())
                            .unwrap_or_else(|| id.to_string());
                        let cached = profiles.lock().unwrap().get(&email).cloned();
                        let image = if let Some(image) = cached {
                            image
                        } else {
                            if misses >= 4 {
                                continue;
                            }
                            misses += 1;
                            let image = client.author(&id.to_string()).and_then(|bytes| {
                                remote::requests::image_format(&bytes)
                                    .map(|format| Arc::new(Image::from_bytes(format, bytes)))
                            });
                            let mut profiles = profiles.lock().unwrap();
                            if profiles.len() >= 256 {
                                profiles.clear();
                            }
                            // A local/unpublished SHA can be absent on the provider.
                            // Cache that miss by commit, without hiding the same
                            // author's photo on a later, published commit.
                            if image.is_some() {
                                profiles.insert(email, image.clone());
                            }
                            image
                        };
                        images.push((id, image));
                    }
                    images
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                if v.avatars.path != request_path
                    || v.engine.repository_path() != request_path
                    || !Arc::ptr_eq(&v.avatars.profiles, &request_profiles)
                {
                    return;
                }
                v.avatars.pending = false;
                for (id, image) in images {
                    v.avatars.attempted.insert(id);
                    if let Some(image) = image {
                        v.avatars.images.insert(id, image);
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
