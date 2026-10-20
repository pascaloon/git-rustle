//! Opt-in General settings fixtures. Network checks stay on loopback and installs are disabled.
use super::*;
use std::{
    cell::RefCell,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

struct Fixture {
    api: String,
    requests: Arc<AtomicUsize>,
    downloads: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

const PHOTO: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x04\x00\x00\x00\xb5\x1c\x0c\x02\x00\x00\x00\x0bIDAT\x78\xda\x63\xfc\xff\x1f\x00\x03\x03\x02\x00\xef\xa3\xf9\x84\x00\x00\x00\x00IEND\xae\x42\x60\x82";
impl Fixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback update fixture");
        listener.set_nonblocking(true).unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let release = |version: &str| {
            serde_json::json!({
            "tag_name":format!("v{version}"), "name":version,
            "published_at":"2026-10-19T00:00:00Z", "created_at":"2026-10-19T00:00:00Z",
            "draft":false, "prerelease":false,
            "assets":[{"name":crate::update::ASSET,"url":format!("{api}/asset"),"digest":format!("sha256:{}","0".repeat(64))}]
        }).to_string()
        };
        let current = release(crate::update::VERSION);
        let available = release("99.0.0");
        let requests = Arc::new(AtomicUsize::new(0));
        let downloads = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let counts = requests.clone();
        let assets = downloads.clone();
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                        break;
                    }
                }
                let sample = counts.fetch_add(1, Ordering::AcqRel);
                if request.starts_with("GET /asset ") {
                    assets.fetch_add(1, Ordering::AcqRel);
                }
                let (status, body) = match sample {
                    0 => ("200 OK", current.as_str()),
                    1 => ("200 OK", available.as_str()),
                    _ => ("502 Bad Gateway", "fixture unavailable"),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        Self {
            api,
            requests,
            downloads,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let joined = self.worker.take().unwrap().join();
        if !thread::panicking() {
            joined.unwrap();
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    phase: u8,
    scheduled: bool,
    fixture: Option<Fixture>,
    started: Option<Instant>,
    check_ms: Vec<f64>,
    clear_frame_ms: Option<f64>,
    verified: bool,
    bounds: Rc<RefCell<[Option<Bounds<Pixels>>; 2]>>,
}

impl Desktop {
    pub(super) fn general_scenario_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.scenario.as_deref(),
            Some("general-settings" | "general-tabs")
        ) {
            return;
        }
        if self.scenario.as_deref() == Some("general-settings") {
            let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("isolated app data"));
            assert_eq!(
                std::fs::read_to_string(root.join("cache-benchmark-fixture")).unwrap(),
                "disposable image cache benchmark"
            );
            let cache = crate::remote::requests::PhotoCache::new(std::path::Path::new("")).unwrap();
            assert_eq!(
                cache.stats().unwrap().images,
                0,
                "General fixture needs fresh isolated image cache"
            );
            for image in 0..8 {
                assert!(cache.store_image(
                    &format!("https://images.example.invalid/general-{image}.png"),
                    PHOTO
                ));
            }
            let fixture = Fixture::new();
            self.updates
                .set_check_fixture(fixture.api.clone(), root.join("general-update-state.json"));
            self.general_bench.fixture = Some(fixture);
        }
        self.general_bench.phase = 1;
        self.engine.mark("general settings requested");
        self.action(Action::Settings, window, cx);
    }

    pub(super) fn general_scenario_schedule(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            self.scenario.as_deref(),
            Some("general-settings" | "general-tabs")
        ) || !(1..=8).contains(&self.general_bench.phase)
            || self.general_bench.scheduled
        {
            return;
        }
        self.general_bench.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.general_bench.scheduled = false;
                v.general_scenario_tick(window, cx);
                // Keep diagnostic waits at paint cadence instead of the normal
                // background maintenance timer's 250 ms cadence.
                if (1..=8).contains(&v.general_bench.phase) {
                    cx.notify();
                }
            });
        });
    }

    pub(super) fn general_settings_probe(&self, slot: usize) -> Option<AnyElement> {
        if !matches!(
            self.scenario.as_deref(),
            Some("general-settings" | "general-tabs")
        ) {
            return None;
        }
        let bounds = self.general_bench.bounds.clone();
        Some(
            canvas(
                move |bounds, _, _| bounds,
                move |value, _, _, _| {
                    bounds.borrow_mut()[slot] = Some(value);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    fn general_begin_check(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        assert!(
            self.updates.check_deferred(),
            "manual check must bypass the scheduled timer"
        );
        self.general_bench.phase = phase;
        self.general_bench.started = Some(Instant::now());
        self.engine.mark(match phase {
            4 => "general current check requested",
            6 => "general available check requested",
            _ => "general error check requested",
        });
        self.action(Action::CheckUpdates, window, cx);
        assert!(!self.update_check_enabled());
        // Repeating the actual action while disabled must not create another request.
        self.action(Action::CheckUpdates, window, cx);
    }

    pub(super) fn general_scenario_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.scenario.as_deref(),
            Some("settings-cache" | "settings-cache-clear")
        ) && self.engine.app.settings.open
            && !self.engine.app.settings.busy
            && self.engine.app.settings.page != crate::settings::GENERAL_PAGE
        {
            self.action(
                Action::SettingsTab(crate::settings::GENERAL_PAGE),
                window,
                cx,
            );
        }
        if !matches!(
            self.scenario.as_deref(),
            Some("general-settings" | "general-tabs")
        ) {
            return;
        }
        match self.general_bench.phase {
            1 if !self.engine.app.settings.busy => {
                self.action(
                    Action::SettingsTab(crate::settings::GENERAL_PAGE),
                    window,
                    cx,
                );
                if self.scenario.as_deref() == Some("general-tabs") {
                    self.scroll_step = 1;
                    self.frame_start = Some(Instant::now());
                    self.general_bench.phase = 10;
                } else {
                    self.general_bench.phase = 2;
                }
                cx.notify();
            }
            2 if !self.image_cache.busy && self.image_cache.stats.is_some() => {
                assert_eq!(self.image_cache.stats.unwrap().images, 8);
                self.general_bench.started = Some(Instant::now());
                self.general_bench.phase = 3;
                self.action(Action::ClearImages, window, cx);
            }
            3 if !self.image_cache.busy && self.image_cache.cleared => {
                self.general_bench.phase = 9;
                let weak = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    let _ = weak.update(cx, |v, cx| {
                        v.general_bench.clear_frame_ms = v
                            .general_bench
                            .started
                            .take()
                            .map(|t| t.elapsed().as_secs_f64() * 1000.);
                        v.engine.mark("general cache clear frame");
                        v.general_begin_check(4, window, cx);
                    });
                });
                cx.notify();
            }
            phase @ (4 | 6 | 8) if self.update_check_enabled() => {
                match (&self.updates.check_state, phase) {
                    (update::CheckState::Current, 4) => {}
                    (update::CheckState::Available(version), 6) => assert_eq!(version, "99.0.0"),
                    (update::CheckState::Failed(error), 8) => {
                        assert!(error.contains("502"), "{error}")
                    }
                    (actual, _) => {
                        panic!("unexpected manual update outcome at phase {phase}: {actual:?}")
                    }
                }
                self.general_bench.phase = 9;
                let weak = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    let _ = weak.update(cx, |v, cx| {
                        v.general_bench.check_ms.push(
                            v.general_bench
                                .started
                                .take()
                                .unwrap()
                                .elapsed()
                                .as_secs_f64()
                                * 1000.,
                        );
                        v.engine.mark(match phase {
                            4 => "general current check frame",
                            6 => "general available check frame",
                            _ => "general error check frame",
                        });
                        if phase == 8 {
                            v.general_bench.phase = 10;
                        } else {
                            v.general_begin_check(phase + 2, window, cx);
                        }
                        cx.notify();
                    });
                });
                cx.notify();
            }
            _ => {}
        }
    }

    pub(super) fn general_scenario_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = [crate::settings::GENERAL_PAGE, 0, 1, 2][(self.scroll_step - 1) % 4];
        self.action(Action::SettingsTab(page), window, cx);
    }

    pub(super) fn general_scenario_ready(&self) -> Option<bool> {
        match self.scenario.as_deref() {
            Some("general-settings") => Some(self.general_bench.phase == 10),
            Some("general-tabs") => Some(self.general_bench.phase == 10 && self.scroll_step > 121),
            _ => None,
        }
    }

    pub(super) fn general_scenario_assert(&mut self) {
        if !matches!(
            self.scenario.as_deref(),
            Some("general-settings" | "general-tabs")
        ) {
            return;
        }
        assert_eq!(self.engine.app.settings.page, crate::settings::GENERAL_PAGE);
        let bounds = self.general_bench.bounds.borrow();
        let updates = bounds[0].expect("Updates card rendered in General");
        let cache = bounds[1].expect("Images cache card rendered in General");
        assert!(updates.size.height > px(0.) && cache.size.height > px(0.));
        assert!(cache.top() >= updates.bottom());
        if self.scenario.as_deref() == Some("general-settings") {
            let fixture = self.general_bench.fixture.as_ref().unwrap();
            assert_eq!(fixture.requests.load(Ordering::Acquire), 3);
            assert_eq!(fixture.downloads.load(Ordering::Acquire), 0);
            assert_eq!(self.general_bench.check_ms.len(), 3);
            assert_eq!(self.image_cache.stats.unwrap().images, 0);
            assert!(self.image_cache.cleared && !self.image_cache.busy);
            assert!(!self.updates.open && !self.updates.installing);
        } else {
            assert_eq!(self.frame_samples.len(), 121);
        }
        self.general_bench.verified = true;
    }

    pub(super) fn general_scenario_summary(&self) -> String {
        let (requests, downloads) = self.general_bench.fixture.as_ref().map_or((0, 0), |f| {
            (
                f.requests.load(Ordering::Acquire),
                f.downloads.load(Ordering::Acquire),
            )
        });
        format!(
            "settings_page={} general_verified={} general_update_requests={requests} general_update_downloads={downloads} general_checks_ms={} general_cache_clear_frame_ms={:?}\n{}",
            self.engine.app.settings.page,
            self.general_bench.verified,
            serde_json::to_string(&self.general_bench.check_ms).unwrap(),
            self.general_bench.clear_frame_ms,
            self.update_check_summary()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Fixture, Ordering, PHOTO};

    #[test]
    fn native_image_seed_fits_the_real_cache_and_clears_completely() {
        let dir = tempfile::tempdir().unwrap();
        let cache = crate::remote::requests::PhotoCache::at(dir.path().join("images"));
        for image in 0..8 {
            assert!(cache.store_image(
                &format!("https://images.example.invalid/general-{image}.png"),
                PHOTO
            ));
        }
        assert_eq!(cache.stats().unwrap().images, 8);
        assert_eq!(cache.clear().unwrap().images, 0);
    }

    #[test]
    fn loopback_check_fixture_exercises_current_available_and_error_without_downloads() {
        let fixture = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        let time = 100_000;
        let current =
            crate::update::check_and_record_at(&fixture.api, crate::update::VERSION, &path, time)
                .unwrap();
        assert!(current.available.is_none());
        let available = crate::update::check_and_record_at(
            &fixture.api,
            crate::update::VERSION,
            &path,
            time + 1,
        )
        .unwrap();
        assert_eq!(available.available.unwrap().version, "99.0.0");
        assert!(
            crate::update::check_and_record_at(
                &fixture.api,
                crate::update::VERSION,
                &path,
                time + 2
            )
            .is_err()
        );
        assert_eq!(fixture.requests.load(Ordering::Acquire), 3);
        assert_eq!(fixture.downloads.load(Ordering::Acquire), 0);
    }
}
