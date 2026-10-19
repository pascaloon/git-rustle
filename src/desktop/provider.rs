//! Provider data arrives on a worker; visible-row rendering only reads the index.
use super::*;
use crate::git::RefKind;
use crate::remote::requests::integration::{self, Association, Index, Project};
use crate::remote::{self, Provider, Remote, requests::Client};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

struct Entry {
    remote: Remote,
    project: Project,
    requests: Index,
    image: Option<Arc<Image>>,
}

#[derive(Default)]
pub(super) struct State {
    path: Option<PathBuf>,
    entries: BTreeMap<String, Entry>,
    upstreams: BTreeMap<String, (String, String)>,
    requested: Vec<Remote>,
    pending: bool,
    refreshed: Option<Instant>,
    session: Arc<()>,
    seeded: bool,
    force: bool,
    rendered_badges: std::cell::Cell<usize>,
}

impl State {
    /// Reconfiguration/refresh invalidates in-flight results as well as badges.
    pub(super) fn invalidate(&mut self) {
        *self = Self {
            path: self.path.clone(),
            force: true,
            ..Self::default()
        };
    }

    fn branch<'a>(&'a self, kind: RefKind, name: &'a str) -> Option<(&'a Entry, &'a str)> {
        if self.seeded
            && let Some((remote, branch)) = self.upstreams.get(name)
        {
            return self
                .entries
                .get(remote)
                .map(|entry| (entry, branch.as_str()));
        }
        match kind {
            RefKind::Remote => self.entries.iter().rev().find_map(|(remote, entry)| {
                name.strip_prefix(remote)?
                    .strip_prefix('/')
                    .map(|branch| (entry, branch))
            }),
            RefKind::Branch => {
                let (remote, branch) = self.upstreams.get(name)?;
                self.entries
                    .get(remote)
                    .map(|entry| (entry, branch.as_str()))
            }
            _ => None,
        }
    }

    fn preferred(&self) -> Option<&Entry> {
        self.entries
            .get("origin")
            .or_else(|| self.entries.values().next())
    }
}

fn request_label(provider: Provider, association: &Association) -> String {
    format!(
        "{} {}{}",
        if provider == Provider::Github {
            "PR"
        } else {
            "MR"
        },
        if provider == Provider::Github {
            "#"
        } else {
            "!"
        },
        association.number
    )
}

impl Desktop {
    pub(super) fn sync_provider(&mut self, cx: &mut Context<Self>) {
        let path = self
            .engine
            .repository_root()
            .map(std::path::Path::to_path_buf);
        if self.provider.path != path {
            self.provider = State {
                path: path.clone(),
                ..State::default()
            };
        }
        if self.provider.seeded || self.scenario.is_some() || !self.engine.has_presented() {
            return;
        }
        let Some(path) = path else { return };
        // Discovery is already complete; this does not read repository config on UI.
        if self.provider.pending
            || (self
                .engine
                .app
                .branches
                .remotes
                .values()
                .map(|u| &u.remote)
                .eq(self.provider.requested.iter())
                && self
                    .provider
                    .refreshed
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(300)))
        {
            return;
        }
        let remotes: Vec<_> = self
            .engine
            .app
            .branches
            .remotes
            .values()
            .map(|u| u.remote.clone())
            .collect();
        self.provider.pending = true;
        self.provider.requested = remotes.clone();
        let session = self.provider.session.clone();
        let no_cache = self.engine.no_cache();
        let force = self.provider.force;
        self.provider.force = false;
        self.engine.mark("provider summary requested");
        cx.spawn(async move |view, cx| {
            let request_path = path.clone();
            let config_path = path.clone();
            let (remotes, upstreams) = cx
                .background_executor()
                .spawn(async move {
                    let (configured, _, upstreams) =
                        remote::configured(&config_path).unwrap_or_default();
                    let upstreams = upstreams
                        .into_iter()
                        .filter_map(|(branch, (remote, merge))| {
                            Some((
                                branch,
                                (remote?, merge?.strip_prefix("refs/heads/")?.into()),
                            ))
                        })
                        .collect::<BTreeMap<_, _>>();
                    let remotes = if configured.is_empty() {
                        remotes
                    } else {
                        configured
                    };
                    (remotes, upstreams)
                })
                .await;
            // Browser links only need config: publish them before any HTTP wait.
            let current = view
                .update(cx, |v, cx| {
                    if v.provider.path.as_ref() != Some(&request_path)
                        || !Arc::ptr_eq(&v.provider.session, &session)
                    {
                        return false;
                    }
                    v.provider.upstreams = upstreams;
                    v.provider
                        .entries
                        .retain(|name, _| remotes.iter().any(|r| &r.name == name));
                    for remote in &remotes {
                        v.provider
                            .entries
                            .entry(remote.name.clone())
                            .or_insert_with(|| Entry {
                                remote: remote.clone(),
                                project: Project::default(),
                                requests: Index::new(),
                                image: None,
                            });
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            for remote in remotes {
                let load_path = path.clone();
                let entry = cx
                    .background_executor()
                    .spawn(async move {
                        let mut project = Project::default();
                        let mut requests = Index::new();
                        let mut image = None;
                        if let Ok(mut client) = Client::new(&load_path, &remote, no_cache) {
                            project = client.project(force).unwrap_or_default();
                            // A failed list does not create misleading stale 'open' badges.
                            requests = client.open_requests(force).unwrap_or_default();
                            image = project
                                .avatar
                                .as_deref()
                                .and_then(|u| client.avatar(u))
                                .and_then(|bytes| {
                                    remote::requests::image_format(&bytes)
                                        .map(|format| Arc::new(Image::from_bytes(format, bytes)))
                                });
                        }
                        Entry {
                            remote,
                            project,
                            requests,
                            image,
                        }
                    })
                    .await;
                let current = view
                    .update(cx, |v, cx| {
                        if v.provider.path.as_ref() != Some(&request_path)
                            || !Arc::ptr_eq(&v.provider.session, &session)
                        {
                            return false;
                        }
                        v.provider.entries.insert(entry.remote.name.clone(), entry);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
            }
            let _ = view.update(cx, |v, cx| {
                if v.provider.path.as_ref() != Some(&request_path)
                    || !Arc::ptr_eq(&v.provider.session, &session)
                {
                    return;
                }
                v.provider.pending = false;
                v.provider.refreshed = Some(Instant::now());
                v.engine.mark("provider summary ready");
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn provider_icon(&self) -> Option<Stateful<Div>> {
        let entry = self.provider.preferred()?;
        let image = entry.image.as_ref()?;
        let title = entry.remote.project.clone();
        Some(
            div()
                .id("project-icon")
                .mr_3()
                .child(requests::avatar(Some(image), &title, 42.))
                .tooltip(move |w, cx| {
                    gpui_component::tooltip::Tooltip::new(title.clone()).build(w, cx)
                }),
        )
    }

    pub(super) fn provider_badge(
        &self,
        kind: RefKind,
        name: &str,
        cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        let (entry, branch) = self.provider.branch(kind, name)?;
        let requests = entry.requests.get(branch)?;
        let request = requests.first()?;
        let mut label = request_label(entry.remote.provider, request);
        if requests.len() > 1 {
            label.push_str(&format!(" +{}", requests.len() - 1));
        }
        let title = requests
            .iter()
            .map(|r| format!("{} · {}", request_label(entry.remote.provider, r), r.title))
            .collect::<Vec<_>>()
            .join("\n");
        let url = request.url.clone();
        self.provider
            .rendered_badges
            .set(self.provider.rendered_badges.get() + 1);
        Some(
            div()
                .id(SharedString::from(format!(
                    "request-{}-{name}",
                    entry.remote.name
                )))
                .px_1()
                .rounded_sm()
                .flex_shrink_0()
                .text_size(px(12.))
                .text_color(rgb(ACCENT_HOVER))
                .bg(rgb(REMOTE_BG))
                .cursor_pointer()
                .child(label)
                .tooltip(move |w, cx| {
                    gpui_component::tooltip::Tooltip::new(title.clone()).build(w, cx)
                })
                .on_click(cx.listener(move |v, _, w, cx| {
                    v.action(Action::OpenProviderUrl(url.clone()), w, cx);
                    cx.stop_propagation();
                })),
        )
    }

    fn provider_branch_items(
        &self,
        kind: RefKind,
        name: &str,
        items: &mut Vec<(Action, &'static str, String, bool)>,
    ) {
        let Some((entry, branch)) = self.provider.branch(kind, name) else {
            return;
        };
        if let Some(url) =
            integration::branch_url(&entry.remote, entry.project.web_url.as_deref(), branch)
        {
            items.push((
                Action::OpenProviderUrl(url),
                "cloud",
                format!("Open {name} online"),
                true,
            ));
        }
        if let Some(requests) = entry.requests.get(branch) {
            for request in requests {
                items.push((
                    Action::OpenProviderUrl(request.url.clone()),
                    "branch",
                    format!(
                        "Open {} · {name}",
                        request_label(entry.remote.provider, request)
                    ),
                    true,
                ));
            }
        }
    }

    pub(super) fn provider_commit_items(
        &self,
        items: &mut Vec<(Action, &'static str, String, bool)>,
    ) {
        let app = &self.engine.app;
        if app.pending.selected || self.selected_commits.ids(app.selected_id()).len() != 1 {
            return;
        }
        let Some(id) = app.selected_id() else { return };
        let source = app.ghost_sources.get(&id);
        let entry = source
            .and_then(|remote| self.provider.entries.get(&remote.name))
            .or_else(|| {
                app.labels
                    .get(&id)
                    .and_then(|labels| {
                        labels.iter().find_map(|label| {
                            self.provider
                                .branch(label.kind, &label.name)
                                .map(|(entry, _)| entry)
                        })
                    })
                    .or_else(|| self.provider.preferred())
            });
        let remote = source.or_else(|| entry.map(|entry| &entry.remote));
        if let Some(remote) = remote
            && let Some(url) = integration::commit_url(
                remote,
                entry
                    .filter(|e| &e.remote == remote)
                    .and_then(|e| e.project.web_url.as_deref()),
                &id.to_string(),
            )
        {
            items.push((
                Action::OpenProviderUrl(url),
                "cloud",
                "Open commit online".into(),
                true,
            ));
        }
        if let Some(labels) = app.labels.get(&id) {
            for label in labels {
                self.provider_branch_items(label.kind, &label.name, items);
            }
        }
    }

    pub(super) fn provider_branch_menu(
        &self,
        position: Point<Pixels>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut items = Vec::new();
        if let Some(entry) = self.engine.app.branches.chosen() {
            let target = &entry.target;
            items.push((
                Action::NewRequest(target.name.clone()),
                "branch",
                "Create PR / MR…".into(),
                true,
            ));
            let kind = if target.remote.is_some() {
                RefKind::Remote
            } else {
                RefKind::Branch
            };
            self.provider_branch_items(kind, &target.label(), &mut items);
        }
        let height =
            ((items.len() * 36 + 10) as f32).min(f32::from(window.viewport_size().height) - 16.);
        let x = position
            .x
            .min(window.viewport_size().width - px(320.))
            .max(px(0.));
        let y = position
            .y
            .min(window.viewport_size().height - px(height))
            .max(px(0.));
        deferred(
            column()
                .id("branch-provider-menu")
                .absolute()
                .left(x)
                .top(y)
                .w(px(320.))
                .max_h(px(height))
                .overflow_y_scroll()
                .p_1()
                .rounded_md()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(LINE))
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                    v.context_menu = None;
                    cx.notify();
                }))
                .children(items.into_iter().enumerate().map(
                    |(i, (action, icon_name, label, _))| {
                        row()
                            .id(("branch-provider-menu-item", i))
                            .h(px(36.))
                            .px_2()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(SELECTED_HOVER)))
                            .child(icon(icon_name))
                            .child(label)
                            .on_click(
                                cx.listener(move |v, _, w, cx| v.action(action.clone(), w, cx)),
                            )
                    },
                )),
        )
        .into_any_element()
    }

    pub(super) fn seed_provider_scenario(&mut self) {
        let remote =
            Remote::parse("origin", "https://github.com/benchmark/project.git", None).unwrap();
        let mut entry = Entry {
            remote,
            project: Project::default(),
            requests: Index::new(),
            image: Some(Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../assets/app-icon.png").to_vec(),
            ))),
        };
        let mut upstreams = BTreeMap::new();
        // Dense fixture: every rendered commit has a populated request badge.
        // Shared names bound the index to 10k while retaining the full graph size.
        for (n, commit) in self.engine.app.history.commits.iter().enumerate() {
            let name = format!("benchmark/{}", n % 10000);
            self.engine.app.labels.entry(commit.id).or_default().insert(
                0,
                crate::app::Label {
                    name,
                    kind: RefKind::Branch,
                    remote_only: false,
                },
            );
        }
        for labels in self.engine.app.labels.values() {
            for label in labels {
                if matches!(label.kind, RefKind::Branch | RefKind::Remote) {
                    // Fixture maps every visible label explicitly; no repository refs are written.
                    upstreams.insert(label.name.clone(), ("origin".into(), label.name.clone()));
                    entry.requests.insert(
                        label.name.clone(),
                        vec![Association {
                            number: 42,
                            title: "Benchmark associated request".into(),
                            url: "https://github.com/benchmark/project/pull/42".into(),
                        }],
                    );
                }
            }
        }
        for n in 0..10000 {
            entry
                .requests
                .entry(format!("benchmark/{n}"))
                .or_insert_with(|| {
                    vec![Association {
                        number: n + 100,
                        title: "Synthetic request".into(),
                        url: format!("https://github.com/benchmark/project/pull/{}", n + 100),
                    }]
                });
        }
        self.provider = State {
            path: self
                .engine
                .repository_root()
                .map(std::path::Path::to_path_buf),
            entries: [("origin".into(), entry)].into_iter().collect(),
            upstreams,
            seeded: true,
            ..State::default()
        };
    }

    pub(super) fn provider_scenario_summary(&self) -> String {
        format!(
            "provider_seeded={} provider_requests={} provider_images={} provider_badges_rendered={}",
            self.provider.seeded,
            self.provider
                .entries
                .values()
                .map(|e| e.requests.len())
                .sum::<usize>(),
            self.provider
                .entries
                .values()
                .filter(|e| e.image.is_some())
                .count(),
            self.provider.rendered_badges.get()
        )
    }

    pub(super) fn provider_scenario_ready(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        self.provider.rendered_badges.get() > 0
            && self
                .provider
                .preferred()
                .and_then(|e| e.image.as_ref())
                .is_some_and(|image| image.clone().get_render_image(window, cx).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::{Arc, Association, Entry, Instant, Project, RefKind, Remote, State};

    fn state() -> State {
        let remote =
            Remote::parse("upstream", "https://github.com/team/project.git", None).unwrap();
        let requests = (0..10000)
            .map(|n| {
                (
                    format!("feature/{n}"),
                    vec![Association {
                        number: n + 1,
                        title: "Benchmark request".into(),
                        url: format!("https://github.com/team/project/pull/{}", n + 1),
                    }],
                )
            })
            .collect();
        State {
            entries: [(
                "upstream".into(),
                Entry {
                    remote,
                    project: Project::default(),
                    requests,
                    image: None,
                },
            )]
            .into_iter()
            .collect(),
            upstreams: [(
                "local-name".into(),
                ("upstream".into(), "feature/42".into()),
            )]
            .into_iter()
            .collect(),
            ..State::default()
        }
    }

    #[test]
    fn local_branch_uses_configured_upstream_and_unpublished_branch_has_no_badge() {
        let state = state();
        let (entry, branch) = state.branch(RefKind::Branch, "local-name").unwrap();
        assert_eq!(entry.remote.name, "upstream");
        assert_eq!(branch, "feature/42");
        assert_eq!(entry.requests[branch][0].number, 43);
        assert!(state.branch(RefKind::Branch, "feature/42").is_none());
        let (entry, branch) = state
            .branch(RefKind::Remote, "upstream/feature/42")
            .unwrap();
        assert_eq!(entry.requests[branch][0].number, 43);
        assert!(state.branch(RefKind::Remote, "origin/feature/42").is_none());
    }

    #[test]
    fn invalidation_discards_stale_repository_results() {
        let mut state = state();
        let session = state.session.clone();
        state.pending = true;
        state.invalidate();
        assert!(!Arc::ptr_eq(&session, &state.session));
        assert!(state.entries.is_empty());
        assert!(!state.pending);
        assert!(state.force);
    }

    #[test]
    fn remote_names_containing_slashes_use_longest_configured_prefix() {
        let mut state = state();
        let mut entry = state.entries.remove("upstream").unwrap();
        entry.remote.name = "company/upstream".into();
        state.entries.insert(entry.remote.name.clone(), entry);
        let (entry, branch) = state
            .branch(RefKind::Remote, "company/upstream/feature/42")
            .unwrap();
        assert_eq!(entry.remote.name, "company/upstream");
        assert_eq!(branch, "feature/42");
    }

    #[test]
    #[ignore = "visible-row provider index lookup component timing"]
    fn benchmark_provider_lookup() {
        let state = state();
        let labels: Vec<_> = (0..40)
            .map(|n| format!("upstream/feature/{}", n * 251))
            .collect();
        let mut samples = Vec::new();
        for sample in 0..16 {
            let started = Instant::now();
            for _ in 0..200 {
                for label in &labels {
                    let (entry, branch) = state
                        .branch(RefKind::Remote, std::hint::black_box(label))
                        .unwrap();
                    assert!(std::hint::black_box(entry.requests.get(branch)).is_some());
                }
            }
            let elapsed = started.elapsed().as_secs_f64() * 1000. / 200.;
            if sample != 0 {
                samples.push(elapsed);
            }
        }
        println!("provider_40_visible_labels_index_ms_per_frame={samples:?}");
    }
}
