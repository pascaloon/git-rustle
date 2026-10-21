//! Lazy, independently expandable details for the explicit commit selection.
use super::*;
use std::collections::{HashMap, HashSet};

struct Header {
    id: gix::ObjectId,
    subject: String,
}
#[derive(Default)]
pub(super) struct State {
    repository: String,
    epoch: u64,
    ids: Vec<gix::ObjectId>,
    headers: Vec<Header>,
    expanded: HashSet<gix::ObjectId>,
    inflight: HashSet<gix::ObjectId>,
    documents: HashMap<gix::ObjectId, Entity<TextBlock>>,
    errors: HashMap<gix::ObjectId, String>,
    graph_version: Option<(usize, usize, Option<u64>)>,
    query: String,
}
impl State {
    fn reconcile(&mut self, ids: &[gix::ObjectId]) -> bool {
        if self.ids == ids {
            return false;
        }
        let entering_multi = self.ids.len() <= 1 && ids.len() > 1;
        self.ids = ids.to_vec();
        self.expanded.retain(|id| ids.contains(id));
        if entering_multi {
            self.expanded.clear();
        }
        if self.expanded.is_empty()
            && let Some(id) = ids.last()
        {
            self.expanded.insert(*id);
        }
        self.documents.retain(|id, _| ids.contains(id));
        self.errors.retain(|id, _| ids.contains(id));
        true
    }
}
impl Desktop {
    pub(super) fn sync_multi_details(&mut self, cx: &mut Context<Self>) {
        if self.selected_commits.count(self.engine.app.selected_id()) <= 1 {
            // The ordinary inspector owns single-selection details. Avoid path
            // clones, selection allocations, and history searches on its render
            // path, including every streaming-history revision.
            if !self.multi_details.ids.is_empty() {
                self.multi_details.epoch = self.multi_details.epoch.wrapping_add(1);
                self.multi_details.inflight.clear();
                self.multi_details.reconcile(&[]);
                self.multi_details.headers.clear();
                self.multi_details.graph_version = None;
            }
            return;
        }
        let repository = self.engine.repository_path();
        if self.multi_details.repository != repository {
            let epoch = self.multi_details.epoch.wrapping_add(1);
            self.multi_details = State {
                repository,
                epoch,
                ..Default::default()
            };
        }
        let ids = self.selected_commits.ids(self.engine.app.selected_id());
        let graph_version = (
            self.engine.app.history.commits.as_ptr() as usize,
            self.engine.app.history.len(),
            self.engine.app.refs.as_ref().map(|r| r.fingerprint),
        );
        let graph_changed = self.multi_details.graph_version != Some(graph_version);
        if graph_changed {
            self.multi_details.graph_version = Some(graph_version);
            self.multi_details.errors.clear();
        }
        if self.multi_details.reconcile(&ids) || graph_changed {
            self.multi_details.headers = ids
                .iter()
                .map(|id| Header {
                    id: *id,
                    subject: self
                        .engine
                        .app
                        .history
                        .index_of(id)
                        .map(|index| self.engine.app.history.subject(index).to_owned())
                        .unwrap_or_else(|| "Commit".into()),
                })
                .collect();
        }
        let query = &self.engine.app.search.text.value;
        if self.multi_details.query != *query {
            self.multi_details.query = query.clone();
            for block in self.multi_details.documents.values() {
                block.update(cx, |text, cx| text.set_query(query, cx));
            }
        }
        if ids.len() > 1 {
            for id in self.multi_details.expanded.clone() {
                self.load_multi_detail(id, cx);
            }
        }
    }
    fn load_multi_detail(&mut self, id: gix::ObjectId, cx: &mut Context<Self>) {
        if self.multi_details.documents.contains_key(&id)
            || self.multi_details.errors.contains_key(&id)
            || !self.multi_details.inflight.insert(id)
        {
            return;
        }
        let ready = self.engine.app.ghosts.get(&id).cloned().or_else(|| {
            self.engine
                .app
                .details
                .as_ref()
                .filter(|d| d.id == id)
                .cloned()
        });
        let repository = self.multi_details.repository.clone();
        let epoch = self.multi_details.epoch;
        let path = std::path::PathBuf::from(&repository);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = match ready {
                Some(details) => Ok(details),
                None => crate::git::open(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|repo| {
                        crate::git::commit_details(&repo, &id).map_err(|e| e.to_string())
                    }),
            };
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |view, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = view.update(cx, |v, cx| {
                    if v.multi_details.epoch != epoch || v.engine.repository_path() != repository {
                        return;
                    }
                    v.multi_details.inflight.remove(&id);
                    if !v.multi_details.ids.contains(&id) {
                        return;
                    }
                    match result {
                        Ok(details) => {
                            let block = cx.new(TextBlock::new);
                            block.update(cx, |s, cx| {
                                s.set_document(
                                    crate::desktop_details::document(&details),
                                    &v.engine.app.search.text.value,
                                    cx,
                                )
                            });
                            v.multi_details.documents.insert(id, block);
                        }
                        Err(error) => {
                            v.multi_details.errors.insert(id, error);
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
    pub(super) fn toggle_multi_detail(&mut self, id: gix::ObjectId, cx: &mut Context<Self>) {
        if !self.multi_details.expanded.remove(&id) {
            self.multi_details.expanded.insert(id);
            self.load_multi_detail(id, cx);
        }
        cx.notify();
    }
    pub(super) fn multi_details_loaded(&self, minimum: usize) -> bool {
        self.multi_details
            .expanded
            .iter()
            .filter(|id| self.multi_details.documents.contains_key(*id))
            .count()
            >= minimum
    }
    pub(super) fn multi_details_panel(&self, cx: &Context<Self>) -> AnyElement {
        column()
            .id("multi-commit-details")
            .relative()
            .children(self.graph_feature_probe(0, 4))
            .h(relative(0.75))
            .flex_shrink_0()
            .min_h_0()
            .overflow_y_scroll()
            .children(
                self.multi_details
                    .headers
                    .iter()
                    .enumerate()
                    .map(|(index, header)| {
                        let id = header.id;
                        let expanded = self.multi_details.expanded.contains(&id);
                        column()
                            .flex_shrink_0()
                            .border_b_1()
                            .border_color(rgb(INSPECTOR_LINE))
                            .child(
                                row()
                                    .id(("commit-accordion", index))
                                    .px_3()
                                    .py_2()
                                    .min_w_0()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .child(
                                        icon(if expanded { "down" } else { "next" }).size(px(14.)),
                                    )
                                    .child(
                                        div()
                                            .font_family("Cascadia Mono")
                                            .text_size(px(11.))
                                            .flex_shrink_0()
                                            .child(id.to_hex_with_len(8).to_string()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(header.subject.clone()),
                                    )
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        v.toggle_multi_detail(id, cx);
                                    })),
                            )
                            .when(expanded, |s| {
                                s.child(
                                    div()
                                        .h(px(200.))
                                        .px_3()
                                        .py_2()
                                        .when_some(
                                            self.multi_details.documents.get(&id),
                                            |s, block| s.child(block.clone()),
                                        )
                                        .when(
                                            !self.multi_details.documents.contains_key(&id),
                                            |s| {
                                                s.child(caption(
                                                    self.multi_details
                                                        .errors
                                                        .get(&id)
                                                        .cloned()
                                                        .unwrap_or_else(|| {
                                                            "Loading commit details…".into()
                                                        }),
                                                ))
                                            },
                                        ),
                                )
                            })
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::State;
    use std::collections::HashSet;
    fn id(n: u8) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&[n; 20])
    }
    #[test]
    fn accordion_selection_preserves_expansion_and_bounds_retained_state() {
        let mut state = State::default();
        assert!(state.reconcile(&[id(1)]));
        state.reconcile(&[id(1), id(2)]);
        assert_eq!(state.expanded, HashSet::from([id(2)]));
        state.expanded.insert(id(1));
        state.errors.insert(id(1), "missing".into());
        assert!(!state.reconcile(&[id(1), id(2)]));
        state.reconcile(&[id(1), id(2), id(3)]);
        assert_eq!(state.expanded, HashSet::from([id(1), id(2)]));
        state.reconcile(&[id(2), id(3)]);
        assert!(!state.errors.contains_key(&id(1)));
        assert!(!state.expanded.contains(&id(1)));
        state.reconcile(&[]);
        assert!(state.expanded.is_empty());
    }
}
