//! Cache controls do filesystem work only on the background executor.
use super::*;
use crate::remote::requests::{PhotoCache, PhotoStats};

#[derive(Default)]
pub(super) struct State {
    pub busy: bool,
    pub clearing: bool,
    pub stats: Option<PhotoStats>,
    pub cleared: bool,
    message: String,
    error: bool,
}

impl Desktop {
    pub(super) fn image_cache_request(&mut self, clear: bool, cx: &mut Context<Self>) {
        if self.image_cache.busy {
            return;
        }
        self.image_cache.busy = true;
        self.image_cache.clearing = clear;
        if clear {
            self.release_provider_images(cx);
        }
        self.image_cache.cleared = false;
        self.image_cache.message = if clear {
            "Clearing…"
        } else {
            "Calculating size…"
        }
        .into();
        self.image_cache.error = false;
        self.engine.mark(if clear {
            "image cache clear requested"
        } else {
            "image cache requested"
        });
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let cache = PhotoCache::new(std::path::Path::new(""))
                        .ok_or_else(|| "Cannot locate the images cache".to_string())?;
                    if clear { cache.clear() } else { cache.stats() }
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                v.image_cache.busy = false;
                v.image_cache.clearing = false;
                match result {
                    Ok(stats) => {
                        v.image_cache.stats = Some(stats);
                        v.image_cache.cleared = clear;
                        v.image_cache.message =
                            if clear { "Images cache cleared." } else { "" }.into();
                        if clear {
                            v.avatars.release_images(cx);
                            v.release_provider_images(cx);
                        }
                        v.engine.mark(if clear {
                            "image cache cleared"
                        } else {
                            "image cache ready"
                        });
                    }
                    Err(error) => {
                        v.image_cache.stats = None;
                        v.image_cache.error = true;
                        v.image_cache.message = format!("Images cache: {error}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn image_cache_controls(&self, cx: &Context<Self>) -> Div {
        let state = &self.image_cache;
        let size = state
            .stats
            .map(|stats| {
                let size = if stats.bytes >= 1024 * 1024 {
                    format!("{:.1} MB", stats.bytes as f64 / (1024. * 1024.))
                } else if stats.bytes >= 1024 {
                    format!("{:.1} KB", stats.bytes as f64 / 1024.)
                } else {
                    format!("{} bytes", stats.bytes)
                };
                format!("{size} · {} cached images", stats.images)
            })
            .unwrap_or_else(|| {
                if state.busy {
                    "Calculating size…"
                } else {
                    "Size unavailable"
                }
                .into()
            });
        column()
            .w_full()
            .gap_1()
            .py_1()
            .child(
                row()
                    .child(
                        column()
                            .flex_1()
                            .gap_1()
                            .child("Image cache")
                            .child(caption(size)),
                    )
                    .child(Button::new("clear-images-cache").small().w(px(164.))
                        .icon(gpui_component::Icon::default().path("rustle/trash"))
                        .label(if state.busy { "Please wait…" } else { "Clear cache" })
                        .tooltip("Remove downloaded author and project images. They download again as needed.")
                        .disabled(state.busy || !state.stats.is_some_and(|stats| stats.images > 0 || stats.bytes > 0))
                        .on_click(cx.listener(|v, _, w, cx| v.action(Action::ClearImages, w, cx)))),
            )
            .when(!state.message.is_empty(), |s| {
                s.child(
                    div()
                        .text_color(rgb(if state.error {
                            palette().removed
                        } else {
                            palette().muted
                        }))
                        .child(state.message.clone()),
                )
            })
    }
}
