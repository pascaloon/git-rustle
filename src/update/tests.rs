use super::*;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

struct Fixture {
    api: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(version: &str, asset: &str, bytes: Vec<u8>, digest: Option<&str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let release = serde_json::json!({
            "tag_name": format!("v{version}"), "name": version,
            "published_at": "2026-10-18T00:00:00Z", "draft": false, "prerelease": false,
            "created_at": "2026-10-18T00:00:00Z",
            "assets": [{ "name": asset, "url": format!("{api}/asset"), "digest": digest }]
        })
        .to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
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
                let body = if request.starts_with("GET /asset ") {
                    bytes.as_slice()
                } else {
                    release.as_bytes()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        Self {
            api,
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

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn detects_only_newer_stable_releases_with_the_exact_windows_asset() {
    for (version, asset, expected) in [
        ("0.2.0", ASSET, true),
        ("0.1.0", ASSET, false),
        ("0.0.9", ASSET, false),
        ("0.2.0-rc.1", ASSET, false),
        ("0.10.0", ASSET, true),
        ("0.2.0", "source.zip", false),
        ("0.2.0", "git-rustle-aarch64-pc-windows-msvc.exe", false),
    ] {
        let fixture = Fixture::new(version, asset, vec![], Some(&digest(b"")));
        let result = check_at(&fixture.api, "0.1.0").unwrap();
        assert_eq!(result.is_some(), expected, "{version}: {asset}");
    }
}

#[test]
fn refuses_missing_or_malformed_release_digests() {
    for digest in [None, Some("sha256:bad"), Some("sha512:1234")] {
        let fixture = Fixture::new("0.2.0", ASSET, vec![], digest);
        assert!(
            check_at(&fixture.api, "0.1.0")
                .unwrap_err()
                .contains("SHA-256")
        );
    }
}

#[test]
fn corrupt_download_and_changed_release_preserve_the_installed_executable() {
    let directory = tempfile::tempdir().unwrap();
    let exe = directory.path().join("git-rustle.exe");
    let expected = digest(b"expected executable");
    for published_digest in [expected.clone(), digest(b"corrupted download")] {
        fs::write(&exe, b"original executable").unwrap();
        let fixture = Fixture::new(
            "0.2.0",
            ASSET,
            b"corrupted download".to_vec(),
            Some(&published_digest),
        );
        let offered = Available {
            version: "0.2.0".into(),
            checksum: expected[7..].into(),
        };
        let error = install_at(&fixture.api, "0.1.0", &offered, &exe).unwrap_err();
        assert!(error.to_lowercase().contains("checksum"), "{error}");
        assert_eq!(fs::read(&exe).unwrap(), b"original executable");
    }
}

#[test]
fn installs_a_verified_raw_executable_to_an_isolated_path() {
    let directory = tempfile::tempdir().unwrap();
    let exe = directory.path().join("git-rustle.exe");
    fs::write(&exe, b"original executable").unwrap();
    let fixture = Fixture::new(
        "0.2.0",
        ASSET,
        b"new executable".to_vec(),
        Some(&digest(b"new executable")),
    );
    let offered = check_at(&fixture.api, "0.1.0").unwrap().unwrap();
    install_at(&fixture.api, "0.1.0", &offered, &exe).unwrap();
    assert_eq!(fs::read(exe).unwrap(), b"new executable");
}

#[test]
fn commit_draft_transfer_preserves_unicode_and_unchecked_files() {
    let draft = CommitDraft {
        description: "A commit 🦀\nSecond line".into(),
        branch: "topic".into(),
        checks: [("a.txt".into(), false)].into(),
    };
    let copy: CommitDraft = serde_json::from_str(&serde_json::to_string(&draft).unwrap()).unwrap();
    assert_eq!(copy.description, draft.description);
    assert_eq!(copy.branch, draft.branch);
    assert_eq!(copy.checks, draft.checks);
}

#[test]
fn malformed_cache_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("updates.json");
    fs::write(&path, b"partial json").unwrap();
    assert_eq!(read_cache(&path).checked, 0);
}

#[test]
fn reopening_preserves_the_six_hour_deadline_and_expiry_fetches_new_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("updates.json");
    let checked = 100_000;
    fs::write(
        &path,
        serde_json::to_vec(&Cache {
            checked,
            available: Some(Available {
                version: "0.2.0".into(),
                checksum: "0".repeat(64),
            }),
        })
        .unwrap(),
    )
    .unwrap();
    for age in [0, 1, 6 * 60 * 60 - 1] {
        // An invalid endpoint proves that fresh-cache checks never contact it.
        let result = check_cached_at("not a URL", "0.1.0", &path, checked + age).unwrap();
        assert_eq!(result.available.unwrap().version, "0.2.0");
        assert_eq!(result.next_check_in, Duration::from_secs(6 * 60 * 60 - age));
    }
    let fixture = Fixture::new("0.3.0", ASSET, vec![], Some(&digest(b"")));
    let result = check_cached_at(&fixture.api, "0.1.0", &path, checked + 6 * 60 * 60).unwrap();
    assert_eq!(result.available.unwrap().version, "0.3.0");
    assert_eq!(result.next_check_in, Duration::from_secs(6 * 60 * 60));
    assert_eq!(read_cache(&path).checked, checked + 6 * 60 * 60);
}

#[test]
fn future_cache_timestamps_are_revalidated_and_failed_checks_preserve_cache() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("updates.json");
    fs::write(
        &path,
        serde_json::to_vec(&Cache {
            checked: 100_001,
            available: None,
        })
        .unwrap(),
    )
    .unwrap();
    let fixture = Fixture::new("0.3.0", ASSET, vec![], Some(&digest(b"")));
    assert!(
        check_cached_at(&fixture.api, "0.1.0", &path, 100_000)
            .unwrap()
            .available
            .is_some()
    );
    let before = fs::read(&path).unwrap();
    assert!(check_cached_at("not a URL", "0.1.0", &path, 100_000 + 6 * 60 * 60).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
#[ignore = "subprocess helper; invoked only by running_executable_can_be_replaced"]
fn replacement_child() {
    let api = std::env::var("GIT_RUSTLE_TEST_UPDATE_API").unwrap();
    let offered = check_at(&api, "0.1.0").unwrap().unwrap();
    install_at(&api, "0.1.0", &offered, &std::env::current_exe().unwrap()).unwrap();
}

#[test]
fn running_executable_can_be_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("running.exe");
    fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
    let bytes = b"verified replacement";
    let fixture = Fixture::new("0.2.0", ASSET, bytes.to_vec(), Some(&digest(bytes)));
    let mut process = command(&child)
        .args(["--exact", "update::tests::replacement_child", "--ignored"])
        .env("GIT_RUSTLE_TEST_UPDATE_API", &fixture.api)
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = process.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            process.kill().unwrap();
            panic!("Replacement subprocess timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read(child).unwrap(), bytes);
}

#[test]
#[ignore = "component benchmark: run after builds finish, retain stdout under target/bench"]
fn benchmark_update() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = fs::read(
        std::env::var_os("GIT_RUSTLE_UPDATE_BENCH_EXE")
            .expect("Set GIT_RUSTLE_UPDATE_BENCH_EXE to the release application executable"),
    )
    .unwrap();
    println!("artifact_bytes={}", bytes.len());
    let checksum = digest(&bytes);
    let fixture = Fixture::new("0.2.0", ASSET, bytes, Some(&checksum));
    for sample in -1..15 {
        let start = std::time::Instant::now();
        let offered = check_at(&fixture.api, "0.1.0").unwrap().unwrap();
        let check_ms = start.elapsed().as_secs_f64() * 1000.;
        let exe = directory.path().join(format!("update-{sample}.exe"));
        fs::write(&exe, b"old executable").unwrap();
        let start = std::time::Instant::now();
        install_at(&fixture.api, "0.1.0", &offered, &exe).unwrap();
        let install_ms = start.elapsed().as_secs_f64() * 1000.;
        println!("{{\"sample\":{sample},\"check_ms\":{check_ms},\"install_ms\":{install_ms}}}");
    }
}

#[test]
#[ignore = "check-only component benchmark; retain stdout under target/bench"]
fn benchmark_check() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = Fixture::new("0.2.0", ASSET, vec![], Some(&digest(b"")));
    let path = directory.path().join("updates.json");
    for sample in -1..15 {
        let start = std::time::Instant::now();
        assert!(check_at(&fixture.api, "0.1.0").unwrap().is_some());
        let metadata_ms = start.elapsed().as_secs_f64() * 1000.;
        let checked = 100_000;
        fs::write(
            &path,
            serde_json::to_vec(&Cache {
                checked,
                available: None,
            })
            .unwrap(),
        )
        .unwrap();
        let start = std::time::Instant::now();
        let result = check_cached_at(&fixture.api, "0.1.0", &path, checked + 1).unwrap();
        assert_eq!(
            result.next_check_in,
            CHECK_INTERVAL - Duration::from_secs(1)
        );
        let cached_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = std::time::Instant::now();
        assert!(
            check_cached_at(
                &fixture.api,
                "0.1.0",
                &path,
                checked + CHECK_INTERVAL.as_secs()
            )
            .unwrap()
            .available
            .is_some()
        );
        let full_ms = start.elapsed().as_secs_f64() * 1000.;
        println!(
            "{{\"sample\":{sample},\"metadata_ms\":{metadata_ms},\"cached_ms\":{cached_ms},\"full_ms\":{full_ms}}}"
        );
    }
}

#[test]
#[ignore = "explicit read-only live GitHub timing; retain stdout and disclose HTTP errors"]
fn benchmark_live_check() {
    for sample in -1..5 {
        let start = std::time::Instant::now();
        let result = check_at(API, VERSION);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
        let outcome = match result {
            Ok(Some(_)) => "update available".to_string(),
            Ok(None) => "no newer compatible release".to_string(),
            Err(error) => error,
        };
        println!(
            "{}",
            serde_json::json!({ "sample": sample, "elapsed_ms": elapsed_ms, "outcome": outcome })
        );
    }
}
