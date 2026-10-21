//! Provider data arrives on a worker; visible-row rendering only reads the index.
use super::*;
use crate::git::RefKind;
use crate::remote::requests::integration::{self, Association, Index, Project};
use crate::remote::requests::pipelines::{self, Pipeline, Status as PipelineStatus};
use crate::remote::{self, Provider, Remote, requests::Client};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
};

struct Entry {
    remote: Remote,
    project: Project,
    requests: Index,
    pipelines: pipelines::Index,
    heads: BTreeMap<String, String>,
    image: Option<Arc<Image>>,
}

enum WorkerEvent {
    Summary(integration::SummaryEvent),
    Pipelines(pipelines::Index),
}

fn same_image(entry: &Entry, bytes: &[u8]) -> bool {
    entry
        .image
        .as_ref()
        .is_some_and(|image| image.bytes() == bytes)
}

#[derive(Default)]
pub(super) struct State {
    path: Option<PathBuf>,
    entries: BTreeMap<String, Entry>,
    upstreams: BTreeMap<String, (String, String)>,
    requested: Vec<Remote>,
    pub(super) pending: bool,
    refreshed: Option<Instant>,
    session: Arc<AtomicU64>,
    seeded: bool,
    force: bool,
    rendered_badges: std::cell::Cell<usize>,
    rendered_pipelines: std::cell::Cell<usize>,
    pipeline_paints: std::rc::Rc<std::cell::Cell<u8>>,
    search_revision: u64,
    search_projection: Option<(u64, u64)>,
}

impl State {
    /// Keep the last usable presentation while replacing the worker session.
    fn refresh(&mut self) {
        self.session.fetch_add(1, AtomicOrdering::AcqRel);
        self.session = Arc::new(AtomicU64::new(0));
        self.pending = false;
        self.refreshed = None;
        self.force = true;
    }

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
            RefKind::Branch | RefKind::Head => {
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

    fn search_labels(
        &self,
        labels: &HashMap<gix::ObjectId, Vec<crate::app::Label>>,
    ) -> HashMap<gix::ObjectId, Vec<String>> {
        let mut out = HashMap::new();
        for (id, labels) in labels {
            let mut values = Vec::new();
            for label in labels {
                let Some((entry, branch)) = self.branch(label.kind, &label.name) else {
                    continue;
                };
                let Some(requests) = entry.requests.get(branch) else {
                    continue;
                };
                for request in requests {
                    values.push(format!(
                        "{} · {}",
                        request_label(entry.remote.provider, request),
                        request.title
                    ));
                }
                if let Some(first) = requests.first()
                    && requests.len() > 1
                {
                    values.push(format!(
                        "{} +{}",
                        request_label(entry.remote.provider, first),
                        requests.len() - 1
                    ));
                }
            }
            if !values.is_empty() {
                values.sort_unstable();
                values.dedup();
                out.insert(*id, values);
            }
        }
        out
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

fn pipeline_appearance(status: PipelineStatus) -> (&'static str, u32) {
    match status {
        PipelineStatus::Pending => ("pipeline-pending", 0x999999),
        PipelineStatus::Running => ("pipeline-running", 0x55aaff),
        PipelineStatus::Success => ("pipeline-success", 0x57c785),
        PipelineStatus::Warning => ("pipeline-warning", 0xe8bf58),
        PipelineStatus::Failed => ("pipeline-failed", 0xf07178),
    }
}

fn current_pipeline<'a>(entry: &'a Entry, branch: &str, id: gix::ObjectId) -> Option<&'a Pipeline> {
    entry.pipelines.get(branch).filter(|pipeline| {
        entry.heads.get(branch) == Some(&pipeline.sha)
            && gix::ObjectId::from_hex(pipeline.sha.as_bytes()).ok() == Some(id)
    })
}

fn deduplicate_provider_items(items: &mut Vec<(Action, &'static str, String, bool)>, start: usize) {
    let mut urls = std::collections::HashSet::new();
    let mut index = 0;
    items.retain(|(action, _, _, _)| {
        let existing = index < start;
        index += 1;
        existing
            || match action {
                Action::OpenProviderUrl(url) => urls.insert(url.clone()),
                _ => true,
            }
    });
}

fn prioritize_header_remote(remotes: &mut [Remote]) {
    // Match State::preferred: a cached header must not wait behind another
    // remote's request-list HTTP timeout.
    let preferred = remotes
        .iter()
        .position(|remote| remote.name == "origin")
        .or_else(|| {
            remotes
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.name.cmp(&b.name))
                .map(|(i, _)| i)
        });
    if let Some(preferred) = preferred {
        remotes.swap(0, preferred);
    }
}

impl Desktop {
    pub(super) fn refresh_provider(&mut self) {
        self.provider.refresh();
    }

    pub(super) fn invalidate_provider_search_labels(&mut self) {
        self.provider.invalidate();
        self.engine.set_provider_search_labels(HashMap::new());
    }

    fn refresh_provider_search_labels(&mut self) {
        let key = (
            self.engine.app.labels_revision(),
            self.provider.search_revision,
        );
        if self.provider.search_projection == Some(key) {
            return;
        }
        if !self.provider.seeded {
            for (name, entry) in &mut self.provider.entries {
                entry.heads = self
                    .engine
                    .app
                    .branches
                    .remotes
                    .get(name)
                    .map(|update| {
                        update
                            .branches
                            .iter()
                            .map(|branch| (branch.name.clone(), branch.sha.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
        let labels = self.provider.search_labels(&self.engine.app.labels);
        self.provider.search_projection = Some(key);
        self.engine.set_provider_search_labels(labels);
    }

    pub(super) fn release_provider_images(&mut self, cx: &mut gpui::App) {
        let mut released = std::collections::HashSet::new();
        for entry in self.provider.entries.values_mut() {
            if let Some(image) = entry.image.take()
                && released.insert(image.id())
            {
                image.remove_asset(cx);
            }
        }
        // A pre-clear download must not restore a removed header image.
        self.provider.session.fetch_add(1, AtomicOrdering::AcqRel);
    }

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
            self.engine.set_provider_search_labels(HashMap::new());
        }
        self.refresh_provider_search_labels();
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
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(60)))
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
        let image_generation = session.load(AtomicOrdering::Acquire);
        let load_images = !self.image_cache.clearing;
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
                    let mut remotes = if configured.is_empty() {
                        remotes
                    } else {
                        configured
                    };
                    prioritize_header_remote(&mut remotes);
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
                    v.provider.entries.retain(|name, entry| {
                        let keep = remotes
                            .iter()
                            .any(|r| &r.name == name && r == &entry.remote);
                        if !keep && let Some(image) = entry.image.take() {
                            image.remove_asset(cx);
                        }
                        keep
                    });
                    for remote in &remotes {
                        v.provider
                            .entries
                            .entry(remote.name.clone())
                            .or_insert_with(|| Entry {
                                remote: remote.clone(),
                                project: Project::default(),
                                requests: Index::new(),
                                pipelines: pipelines::Index::new(),
                                heads: BTreeMap::new(),
                                image: None,
                            });
                    }
                    v.provider.search_revision = v.provider.search_revision.wrapping_add(1);
                    v.refresh_provider_search_labels();
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            for remote in remotes {
                let load_path = path.clone();
                let image_session = session.clone();
                let remote_name = remote.name.clone();
                let (events, incoming) = async_channel::unbounded();
                let worker = cx.background_executor().spawn(async move {
                    if let Ok(mut client) = Client::new(&load_path, &remote, no_cache) {
                        client.summary_stream(
                            force,
                            || {
                                load_images
                                    && image_session.load(AtomicOrdering::Acquire)
                                        == image_generation
                            },
                            |event| events.send_blocking(WorkerEvent::Summary(event)).is_ok(),
                        );
                        let _ = events.send_blocking(WorkerEvent::Pipelines(
                            client.pipelines(force).unwrap_or_default(),
                        ));
                    } else {
                        let _ = events.send_blocking(WorkerEvent::Summary(
                            integration::SummaryEvent::Requests(Index::new()),
                        ));
                    }
                });
                while let Ok(event) = incoming.recv().await {
                    let current = view
                        .update(cx, |v, cx| {
                            if v.provider.path.as_ref() != Some(&request_path)
                                || !Arc::ptr_eq(&v.provider.session, &session)
                            {
                                return false;
                            }
                            let Some(entry) = v.provider.entries.get_mut(&remote_name) else {
                                return false;
                            };
                            match event {
                                WorkerEvent::Summary(integration::SummaryEvent::Project(
                                    project,
                                )) => {
                                    if entry.project.avatar != project.avatar
                                        && let Some(image) = entry.image.take()
                                    {
                                        image.remove_asset(cx);
                                    }
                                    entry.project = project;
                                }
                                WorkerEvent::Summary(integration::SummaryEvent::Image(bytes)) => {
                                    if session.load(AtomicOrdering::Acquire) == image_generation
                                        && !same_image(entry, &bytes)
                                        && let Some(format) = remote::requests::image_format(&bytes)
                                    {
                                        if let Some(old) = entry.image.take() {
                                            old.remove_asset(cx);
                                        }
                                        entry.image =
                                            Some(Arc::new(Image::from_bytes(format, bytes)));
                                        v.engine.mark("provider image delivered");
                                    }
                                }
                                WorkerEvent::Summary(integration::SummaryEvent::Requests(
                                    requests,
                                )) => {
                                    if entry.requests != requests {
                                        entry.requests = requests;
                                        v.provider.search_revision =
                                            v.provider.search_revision.wrapping_add(1);
                                        v.refresh_provider_search_labels();
                                    }
                                }
                                WorkerEvent::Pipelines(pipelines) => entry.pipelines = pipelines,
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                }
                worker.await;
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
        id: gix::ObjectId,
        kind: RefKind,
        name: &str,
        _cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        if kind != RefKind::Remote {
            return None;
        }
        let (entry, branch) = self.provider.branch(kind, name)?;
        if self.engine.app.labels.get(&id).is_some_and(|labels| {
            labels
                .iter()
                .take_while(|label| label.name != name)
                .any(|label| {
                    label.kind == RefKind::Remote
                        && self.provider.branch(label.kind, &label.name).is_some_and(
                            |(other, other_branch)| {
                                other.remote.endpoint == entry.remote.endpoint
                                    && other_branch == branch
                            },
                        )
                })
        }) {
            return None;
        }
        let pipeline = current_pipeline(entry, branch, id);
        let requests = entry
            .requests
            .get(branch)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if pipeline.is_none() && requests.is_empty() {
            return None;
        }
        let mut badges = row()
            .id(SharedString::from(format!(
                "provider-{}-{name}",
                entry.remote.name
            )))
            .gap_1()
            .flex_shrink_0();
        if let Some(pipeline) = pipeline {
            self.provider
                .rendered_pipelines
                .set(self.provider.rendered_pipelines.get() + 1);
            let (shape, color) = pipeline_appearance(pipeline.status);
            let title = format!(
                "Pipeline {} · {} · {}",
                pipeline.id,
                pipeline.status.label(),
                pipeline.detail
            );
            let paints = self.provider.pipeline_paints.clone();
            let bit = 1u8 << pipeline.status as u8;
            badges = badges.child(
                div()
                    .relative()
                    .id(SharedString::from(format!(
                        "pipeline-{}-{name}",
                        entry.remote.name
                    )))
                    .text_color(rgb(color))
                    .text_size(px(15.))
                    .child(icon(shape).size(px(15.)).text_color(rgb(color)))
                    .when(self.provider.seeded, |s| {
                        s.child(
                            canvas(
                                move |bounds, _, _| bounds,
                                move |bounds, _, _, _| {
                                    assert!(
                                        bounds.size.width >= px(14.)
                                            && bounds.size.height >= px(14.)
                                    );
                                    paints.set(paints.get() | bit);
                                },
                            )
                            .absolute()
                            .size_full(),
                        )
                    })
                    .tooltip(move |w, cx| {
                        gpui_component::tooltip::Tooltip::new(title.clone()).build(w, cx)
                    }),
            );
        }
        let Some(request) = requests.first() else {
            return pipeline.map(|_| badges);
        };
        let mut label = request_label(entry.remote.provider, request);
        if requests.len() > 1 {
            label.push_str(&format!(" +{}", requests.len() - 1));
        }
        let title = requests
            .iter()
            .map(|r| format!("{} · {}", request_label(entry.remote.provider, r), r.title))
            .collect::<Vec<_>>()
            .join("\n");
        self.provider
            .rendered_badges
            .set(self.provider.rendered_badges.get() + 1);
        Some(
            badges.child(
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
                    .child(label)
                    .tooltip(move |w, cx| {
                        gpui_component::tooltip::Tooltip::new(title.clone()).build(w, cx)
                    }),
            ),
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
        if let Some(pipeline) = entry
            .pipelines
            .get(branch)
            .filter(|pipeline| entry.heads.get(branch) == Some(&pipeline.sha))
        {
            items.push((
                Action::OpenProviderUrl(pipeline.url.clone()),
                "cloud",
                format!("Open pipeline · {name}"),
                true,
            ));
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
            let start = items.len();
            for label in labels {
                self.provider_branch_items(label.kind, &label.name, items);
            }
            deduplicate_provider_items(items, start);
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
            let target = entry.paired_remotes.first().unwrap_or(&entry.target);
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
            pipelines: pipelines::Index::new(),
            heads: BTreeMap::new(),
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
            if n < 10000 {
                let status = [
                    PipelineStatus::Pending,
                    PipelineStatus::Running,
                    PipelineStatus::Success,
                    PipelineStatus::Warning,
                    PipelineStatus::Failed,
                ][n % 5];
                let sha = commit.id.to_string();
                entry.heads.insert(name.clone(), sha.clone());
                entry.pipelines.insert(
                    name.clone(),
                    Pipeline {
                        id: n as u64 + 1,
                        sha,
                        status,
                        detail: status.label().into(),
                        url: format!(
                            "https://github.com/benchmark/project/actions/runs/{}",
                            n + 1
                        ),
                    },
                );
            }
            self.engine.app.labels.entry(commit.id).or_default().insert(
                0,
                crate::app::Label {
                    name: format!("origin/{name}"),
                    kind: RefKind::Remote,
                    remote_only: false,
                },
            );
        }
        for (id, labels) in &self.engine.app.labels {
            for label in labels {
                if matches!(label.kind, RefKind::Branch | RefKind::Remote) {
                    // Fixture maps every visible label explicitly; no repository refs are written.
                    let branch = label
                        .name
                        .strip_prefix("origin/")
                        .unwrap_or(&label.name)
                        .to_string();
                    upstreams.insert(label.name.clone(), ("origin".into(), branch.clone()));
                    entry.requests.insert(
                        branch.clone(),
                        vec![Association {
                            number: 42,
                            title: "Benchmark associated request".into(),
                            url: "https://github.com/benchmark/project/pull/42".into(),
                        }],
                    );
                    let status = [
                        PipelineStatus::Pending,
                        PipelineStatus::Running,
                        PipelineStatus::Success,
                        PipelineStatus::Warning,
                        PipelineStatus::Failed,
                    ][entry.pipelines.len() % 5];
                    let sha = id.to_string();
                    entry
                        .heads
                        .entry(branch.clone())
                        .or_insert_with(|| sha.clone());
                    entry.pipelines.entry(branch).or_insert_with(|| Pipeline {
                        id: 42,
                        sha,
                        status,
                        detail: status.label().into(),
                        url: "https://github.com/benchmark/project/actions/runs/42".into(),
                    });
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
            "provider_seeded={} provider_requests={} provider_images={} provider_badges_rendered={} provider_pipelines_rendered={} provider_pipeline_paint_mask={}",
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
            self.provider.rendered_badges.get(),
            self.provider.rendered_pipelines.get(),
            self.provider.pipeline_paints.get()
        )
    }

    pub(super) fn provider_scenario_ready(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        self.provider.rendered_badges.get() > 0
            && self.provider.rendered_pipelines.get() > 0
            && self.provider.pipeline_paints.get() == 31
            && self
                .provider
                .preferred()
                .and_then(|e| e.image.as_ref())
                .is_some_and(|image| image.clone().get_render_image(window, cx).is_some())
    }

    pub(super) fn provider_scenario_image(&self) -> Option<Arc<Image>> {
        self.provider
            .preferred()
            .and_then(|entry| entry.image.clone())
    }

    pub(super) fn assert_provider_scenario(&self) {
        assert!(
            self.provider.rendered_pipelines.get() > 0,
            "pipeline icons must actually paint"
        );
        assert_eq!(
            self.provider.pipeline_paints.get(),
            31,
            "all five pipeline status shapes must actually paint"
        );
        let mut items = Vec::new();
        self.provider_commit_items(&mut items);
        let urls: Vec<_> = items
            .iter()
            .filter_map(|(action, _, _, _)| match action {
                Action::OpenProviderUrl(url) => Some(url),
                _ => None,
            })
            .collect();
        let unique: std::collections::HashSet<_> = urls.iter().collect();
        assert_eq!(
            urls.len(),
            unique.len(),
            "local/remote/server aliases must not repeat provider commands"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Arc, Association, Entry, Instant, Project, RefKind, Remote, State};

    #[test]
    fn menus_deduplicate_remote_local_server_aliases_by_destination() {
        let mut items = vec![(
            super::Action::OpenProviderUrl("https://example.invalid/commit/1".into()),
            "cloud",
            "Open commit".into(),
            true,
        )];
        let start = items.len();
        for alias in ["local", "origin/local", "server/local"] {
            for url in [
                "https://example.invalid/tree/local",
                "https://example.invalid/mr/42",
            ] {
                items.push((
                    super::Action::OpenProviderUrl(url.into()),
                    "cloud",
                    format!("Open {alias}"),
                    true,
                ));
            }
        }
        super::deduplicate_provider_items(&mut items, start);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn pipeline_requires_exact_latest_server_head_and_each_status_has_its_own_shape_and_color() {
        let mut state = state();
        let entry = state.entries.get_mut("upstream").unwrap();
        let current = gix::ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
        let old = gix::ObjectId::from_hex("b".repeat(40).as_bytes()).unwrap();
        entry.heads.insert("main".into(), current.to_string());
        entry.pipelines.insert(
            "main".into(),
            super::Pipeline {
                id: 1,
                sha: current.to_string(),
                status: super::PipelineStatus::Running,
                detail: "running".into(),
                url: "https://example.invalid/pipelines/1".into(),
            },
        );
        assert!(super::current_pipeline(entry, "main", current).is_some());
        assert!(super::current_pipeline(entry, "main", old).is_none());
        entry.heads.insert("main".into(), old.to_string());
        assert!(
            super::current_pipeline(entry, "main", current).is_none(),
            "a pipeline on an older head must not decorate fetched origin labels"
        );
        let shapes: std::collections::HashSet<_> = [
            super::PipelineStatus::Pending,
            super::PipelineStatus::Running,
            super::PipelineStatus::Success,
            super::PipelineStatus::Warning,
            super::PipelineStatus::Failed,
        ]
        .into_iter()
        .map(super::pipeline_appearance)
        .collect();
        assert_eq!(shapes.len(), 5);
    }

    #[test]
    fn refresh_retains_image_and_badges_but_rejects_old_worker_session() {
        let mut state = state();
        let bytes = include_bytes!("../../assets/app-icon.png").to_vec();
        let image = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            bytes.clone(),
        ));
        state.entries.get_mut("upstream").unwrap().image = Some(image.clone());
        state.pending = true;
        state.refreshed = Some(Instant::now());
        let old_session = state.session.clone();
        state.refresh();
        assert!(!Arc::ptr_eq(&old_session, &state.session));
        assert!(!state.pending);
        assert!(state.force && state.refreshed.is_none());
        let entry = state.entries.get("upstream").unwrap();
        assert!(Arc::ptr_eq(entry.image.as_ref().unwrap(), &image));
        assert!(super::same_image(entry, &bytes));
        assert!(!super::same_image(entry, b"different image"));
        assert_eq!(entry.requests.len(), 10000);
        assert_eq!(state.upstreams.len(), 1);
        state.invalidate();
        assert!(
            state.entries.is_empty(),
            "reconfiguration must clear old identity"
        );
    }

    #[test]
    fn cached_header_remote_is_processed_before_other_request_lists() {
        for names in [
            vec!["a-slow", "z-other", "origin"],
            vec!["z-slow", "a-header", "m-other"],
            vec!["origin"],
            vec![],
        ] {
            let mut remotes: Vec<_> = names
                .iter()
                .map(|name| {
                    Remote::parse(name, "https://github.com/team/project.git", None).unwrap()
                })
                .collect();
            let state = State {
                entries: remotes
                    .iter()
                    .map(|remote| {
                        (
                            remote.name.clone(),
                            Entry {
                                remote: remote.clone(),
                                project: Project::default(),
                                requests: Default::default(),
                                pipelines: Default::default(),
                                heads: Default::default(),
                                image: None,
                            },
                        )
                    })
                    .collect(),
                ..State::default()
            };
            let preferred = state.preferred().map(|entry| entry.remote.name.as_str());
            super::prioritize_header_remote(&mut remotes);
            assert_eq!(
                remotes.first().map(|remote| remote.name.as_str()),
                preferred
            );
            let mut retained: Vec<_> = remotes.iter().map(|remote| remote.name.as_str()).collect();
            let mut expected = names;
            retained.sort_unstable();
            expected.sort_unstable();
            assert_eq!(retained, expected, "other remote requests must be retained");
        }
    }

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
                    pipelines: Default::default(),
                    heads: Default::default(),
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
        assert!(state.branch(RefKind::Head, "local-name").is_some());
    }

    #[test]
    fn search_projection_includes_all_request_badges_and_tracks_association_removal() {
        let mut state = state();
        state
            .entries
            .get_mut("upstream")
            .unwrap()
            .requests
            .get_mut("feature/42")
            .unwrap()
            .push(Association {
                number: 44,
                title: "Second target".into(),
                url: "https://github.com/benchmark/project/pull/44".into(),
            });
        let id = gix::ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
        let labels = [(
            id,
            vec![
                crate::app::Label {
                    name: "local-name".into(),
                    kind: RefKind::Head,
                    remote_only: false,
                },
                crate::app::Label {
                    name: "upstream/feature/42".into(),
                    kind: RefKind::Remote,
                    remote_only: false,
                },
            ],
        )]
        .into_iter()
        .collect();
        let projected = state.search_labels(&labels);
        assert!(
            projected[&id]
                .iter()
                .any(|value| value.starts_with("PR #43 ·"))
        );
        assert!(
            projected[&id]
                .iter()
                .any(|value| value.starts_with("PR #44 ·"))
        );
        assert!(projected[&id].iter().any(|value| value == "PR #43 +1"));
        assert_eq!(
            projected[&id].len(),
            3,
            "local/upstream aliases must deduplicate"
        );
        state.entries.get_mut("upstream").unwrap().requests.clear();
        assert!(state.search_labels(&labels).is_empty());
    }

    #[test]
    #[ignore = "one metadata projection of 10000 associated graph labels"]
    fn benchmark_provider_search_projection() {
        let state = state();
        let labels = (0..10000)
            .map(|n| {
                let id = gix::ObjectId::from_hex(format!("{:040x}", n + 1).as_bytes()).unwrap();
                (
                    id,
                    vec![crate::app::Label {
                        name: format!("upstream/feature/{n}"),
                        kind: RefKind::Remote,
                        remote_only: false,
                    }],
                )
            })
            .collect();
        for sample in -1..15 {
            let start = Instant::now();
            let projected = state.search_labels(&labels);
            let projection_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(projected.len(), 10000);
            if sample >= 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"projection_ms":projection_ms})
                );
            }
        }
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
