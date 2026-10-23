//! Git smart-HTTP v0 ref discovery, without launching Git or negotiating a pack.
//! See gitprotocol-http(5), "Smart Server Response" and gitprotocol-common(5).
use super::*;
use base64::Engine as _;
use std::collections::HashMap;
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

pub(super) const MIN_HEADS: usize = 4;
type FailedProbes = Mutex<HashMap<(String, u64), Instant>>;
static FAILED: LazyLock<FailedProbes> = LazyLock::new(Default::default);
// Keep live HTTP/TLS connections across clicks. Authorization is attached to
// each request, never to the shared agent. Redirects cannot forward credentials.
static HTTP: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .build()
        .into()
});

/// Derive the clone URL from the trusted provider installation, including for
/// SSH remotes. Never attach a provider token to an arbitrary SSH host/port.
fn endpoint(remote: &Remote) -> Option<String> {
    let api = url::Url::parse(&remote.endpoint).ok()?;
    let root = match remote.provider {
        Provider::Gitlab => remote
            .endpoint
            .split_once("/api/v4/projects/")
            .map(|(base, _)| base),
        Provider::Github if api.origin().ascii_serialization() == "https://api.github.com" => {
            Some("https://github.com")
        }
        Provider::Github => remote
            .endpoint
            .split_once("/api/v3/repos/")
            .map(|(base, _)| base),
    };
    let mut url = if let Some(root) = root {
        let mut url = url::Url::parse(root).ok()?;
        let project = remote
            .project
            .split('/')
            .map(encode)
            .collect::<Vec<_>>()
            .join("/");
        let path = format!("{}/{project}.git", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url
    } else {
        let mut url = url::Url::parse(&remote.url).ok()?;
        if url.origin() != api.origin()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        url.set_username("").ok()?;
        url
    };
    let loopback = (cfg!(test)
        || std::env::var("GIT_RUSTLE_SCENARIO").as_deref() == Ok("refresh-native"))
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some();
    if url.scheme() != "https" && !loopback {
        return None;
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = format!("{}/info/refs", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(Some("service=git-upload-pack"));
    Some(url.into())
}

pub(super) fn read(
    remote: &Remote,
    token: Option<&str>,
    names: &HashSet<String>,
    cancel: &AtomicBool,
) -> Option<Vec<Branch>> {
    read_scope(remote, token, Some(names), cancel)
}

/// Complete branch-browser listing. A final flush is required before publishing
/// any heads, so a failed/truncated advertisement cannot establish deletions.
pub(super) fn read_all(
    remote: &Remote,
    token: Option<&str>,
    cancel: &AtomicBool,
) -> Option<Vec<Branch>> {
    read_scope(remote, token, None, cancel)
}

fn read_scope(
    remote: &Remote,
    token: Option<&str>,
    names: Option<&HashSet<String>>,
    cancel: &AtomicBool,
) -> Option<Vec<Branch>> {
    let url = endpoint(remote)?;
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut identity);
    let key = (url.clone(), identity.finish());
    {
        let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
        failed.retain(|_, at| at.elapsed() < Duration::from_secs(60));
        if failed.contains_key(&key) || cancel.load(Ordering::Relaxed) {
            return None;
        }
    }
    let result = request(&url, remote.provider, token, names, cancel);
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    if result.is_none() {
        FAILED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Instant::now());
    }
    result
}

fn request(
    url: &str,
    provider: Provider,
    token: Option<&str>,
    names: Option<&HashSet<String>>,
    cancel: &AtomicBool,
) -> Option<Vec<Branch>> {
    let mut request = HTTP
        .get(url)
        .header("User-Agent", "git-rustle/0.1")
        .header("Accept", "application/x-git-upload-pack-advertisement")
        .header("Cache-Control", "no-cache");
    if let Some(token) = token {
        let user = if provider == Provider::Gitlab {
            "oauth2"
        } else {
            "x-access-token"
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"));
        request = request.header("Authorization", format!("Basic {encoded}"));
    }
    let result = request.call();
    let mut response = result.ok()?;
    if response.status().as_u16() != 200 || cancel.load(Ordering::Relaxed) {
        return None;
    }
    if response
        .headers()
        .get("content-type")?
        .to_str()
        .ok()?
        .split(';')
        .next()?
        .trim()
        != "application/x-git-upload-pack-advertisement"
    {
        return None;
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_vec()
        .ok()?;
    let branches = parse_scope(&body, names).ok()?;
    Some(branches)
}

fn packet<'a>(bytes: &mut &'a [u8]) -> Result<Option<&'a [u8]>, ()> {
    let prefix = bytes.get(..4).ok_or(())?;
    let size =
        usize::from_str_radix(std::str::from_utf8(prefix).map_err(|_| ())?, 16).map_err(|_| ())?;
    if size == 0 {
        *bytes = &bytes[4..];
        return Ok(None);
    }
    if !(4..=65520).contains(&size) {
        return Err(());
    }
    let value = bytes.get(4..size).ok_or(())?;
    *bytes = &bytes[size..];
    Ok(Some(value))
}

#[cfg(test)]
fn parse(bytes: &[u8], names: &HashSet<String>) -> Result<Vec<Branch>, ()> {
    parse_scope(bytes, Some(names))
}

fn parse_scope(mut bytes: &[u8], names: Option<&HashSet<String>>) -> Result<Vec<Branch>, ()> {
    let service = packet(&mut bytes)?.ok_or(())?;
    if service.strip_suffix(b"\n").unwrap_or(service) != b"# service=git-upload-pack"
        || packet(&mut bytes)?.is_some()
    {
        return Err(());
    }
    let mut first = true;
    let mut empty = false;
    let mut seen = HashSet::new();
    let mut found = BTreeMap::new();
    while let Some(line) = packet(&mut bytes)? {
        let text = std::str::from_utf8(line.strip_suffix(b"\n").unwrap_or(line)).map_err(|_| ())?;
        if empty {
            return Err(());
        }
        let value = if first {
            let (value, caps) = text.split_once('\0').ok_or(())?;
            if caps.chars().any(char::is_control) {
                return Err(());
            }
            value
        } else {
            text
        };
        let (sha, reference) = value.split_once(' ').ok_or(())?;
        gix::ObjectId::from_hex(sha.as_bytes()).map_err(|_| ())?;
        if reference == "capabilities^{}" && first && sha.bytes().all(|b| b == b'0') {
            empty = true;
        } else {
            if sha.bytes().all(|b| b == b'0')
                || reference.bytes().any(|b| b.is_ascii_control() || b == b' ')
                || (reference != "HEAD" && !reference.starts_with("refs/"))
                || !seen.insert(reference)
            {
                return Err(());
            }
            if let Some(name) = reference.strip_prefix("refs/heads/") {
                if name.is_empty() {
                    return Err(());
                }
                if names.is_none_or(|names| names.contains(name)) {
                    found.insert(name.to_owned(), sha.to_owned());
                }
            }
        }
        first = false;
    }
    // A final flush and EOF are required before absence can confirm deletion.
    if first || !bytes.is_empty() {
        return Err(());
    }
    Ok(found
        .into_iter()
        .map(|(name, sha)| Branch { name, sha })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pkt(text: &str) -> Vec<u8> {
        format!("{:04x}{text}", text.len() + 4).into_bytes()
    }
    fn response(lines: &[String]) -> Vec<u8> {
        let mut body = pkt("# service=git-upload-pack\n");
        body.extend(b"0000");
        for line in lines {
            body.extend(pkt(line));
        }
        body.extend(b"0000");
        body
    }
    fn names() -> HashSet<String> {
        ["topic/slash".into(), "deleted".into()]
            .into_iter()
            .collect()
    }
    #[test]
    fn complete_listing_includes_untracked_heads_and_requires_final_flush() {
        let sha = "a".repeat(40);
        let body = response(&[
            format!("{sha} HEAD\0multi_ack\n"),
            format!("{sha} refs/heads/topic/slash\n"),
            format!("{sha} refs/heads/server-only\n"),
            format!("{sha} refs/tags/release\n"),
        ]);
        let all = parse_scope(&body, None).unwrap();
        assert_eq!(
            all.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            vec!["server-only", "topic/slash"]
        );
        assert_eq!(parse(&body, &names()).unwrap().len(), 1);
        assert!(parse_scope(&body[..body.len() - 4], None).is_err());
        let empty = response(&[format!("{} capabilities^{{}}\0multi_ack\n", "0".repeat(40))]);
        assert!(parse_scope(&empty, None).unwrap().is_empty());
    }

    #[test]
    fn parses_heads_ignores_tags_and_requires_a_complete_response() {
        let a = "a".repeat(40);
        let lines = [
            format!("{a} HEAD\0multi_ack symref=HEAD:refs/heads/topic/slash\n"),
            format!("{a} refs/heads/topic/slash\n"),
            format!("{a} refs/heads/unrelated\n"),
            format!("{a} refs/tags/tag\n"),
            format!("{a} refs/tags/tag^{{}}\n"),
        ];
        let body = response(&lines);
        assert_eq!(
            parse(&body, &names()).unwrap(),
            vec![Branch {
                name: "topic/slash".into(),
                sha: a.clone()
            }]
        );
        for end in 0..body.len() {
            assert!(
                parse(&body[..end], &names()).is_err(),
                "truncation at {end}"
            );
        }
        let mut trailing = body.clone();
        trailing.extend(b"0000");
        assert!(parse(&trailing, &names()).is_err());
        for line in [
            format!("{a} refs/heads/topic/slash\n"),
            "not-a-sha refs/heads/other\n".into(),
            format!("{a} refs/heads/bad\0cap\n"),
        ] {
            let mut malformed = lines.to_vec();
            malformed.push(line);
            assert!(parse(&response(&malformed), &names()).is_err());
        }
        assert!(
            parse(
                &response(&[format!("{} capabilities^{{}}\0multi_ack\n", "0".repeat(40))]),
                &names()
            )
            .unwrap()
            .is_empty()
        );
        assert!(parse(&response(&[]), &names()).is_err());
    }
    #[test]
    fn reference_checks_allow_unicode_c1_but_reject_ascii_controls_and_spaces() {
        let a = "a".repeat(40);
        let first = format!("{a} HEAD\0multi_ack\n");
        for code in 0x80..=0x9f {
            let name = format!("topic/{}-branch", char::from_u32(code).unwrap());
            let names = [name.clone()].into_iter().collect();
            let body = response(&[
                first.clone(),
                format!("{a} refs/heads/{name}\n"),
                format!("{a} refs/tags/{}-tag\n", char::from_u32(code).unwrap()),
            ]);
            assert_eq!(
                parse(&body, &names).unwrap(),
                vec![Branch {
                    name,
                    sha: a.clone()
                }]
            );
        }
        for byte in (0u8..=32).chain([127]) {
            let body = response(&[
                first.clone(),
                format!("{a} refs/heads/bad{}name\n", char::from(byte)),
            ]);
            assert!(parse(&body, &names()).is_err());
        }
    }
    #[test]
    fn urls_keep_credentials_on_the_provider_installation() {
        let remote = Remote::parse("origin", "git@gitlab.com:team/repo.git", None).unwrap();
        assert_eq!(
            endpoint(&remote).unwrap(),
            "https://gitlab.com/team/repo.git/info/refs?service=git-upload-pack"
        );
        let github = Remote::parse("origin", "git@github.com:team/repo.git", None).unwrap();
        assert_eq!(
            endpoint(&github).unwrap(),
            "https://github.com/team/repo.git/info/refs?service=git-upload-pack"
        );
        let mut custom = remote.clone();
        custom.url = "ssh://git@ssh.internal:8022/team/repo.git".into();
        custom.endpoint =
            "https://web.internal/gitlab/api/v4/projects/team%2Frepo/repository/branches".into();
        assert_eq!(
            endpoint(&custom).unwrap(),
            "https://web.internal/gitlab/team/repo.git/info/refs?service=git-upload-pack"
        );
        custom.project = "team/literal%20 #name".into();
        assert!(
            endpoint(&custom)
                .unwrap()
                .contains("team/literal%2520%20%23name.git/")
        );
        custom.endpoint = "https://api.internal/custom/branches".into();
        custom.url = "https://untrusted.internal/team/repo.git".into();
        assert!(endpoint(&custom).is_none());
        custom.endpoint =
            "http://web.internal/api/v4/projects/team%2Frepo/repository/branches".into();
        assert!(endpoint(&custom).is_none());
    }
    #[test]
    fn real_git_packet_stream_including_empty_repository_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        for populated in [false, true] {
            if populated {
                command::checked(
                    dir.path(),
                    &[
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                        "commit",
                        "--allow-empty",
                        "-qm",
                        "fixture",
                    ],
                )
                .unwrap();
                command::checked(dir.path(), &["branch", "topic/slash"]).unwrap();
                command::checked(dir.path(), &["tag", "tag"]).unwrap();
                for tag in ["tag-\u{80}", "tag-\u{85}", "tag-\u{9f}"] {
                    command::checked(
                        dir.path(),
                        &["check-ref-format", &format!("refs/tags/{tag}")],
                    )
                    .unwrap();
                    command::checked(dir.path(), &["tag", tag]).unwrap();
                }
            }
            let mut body = pkt("# service=git-upload-pack\n");
            body.extend(b"0000");
            body.extend(
                command::checked(
                    dir.path(),
                    &["upload-pack", "--stateless-rpc", "--advertise-refs", "."],
                )
                .unwrap(),
            );
            let found = parse(&body, &names()).unwrap();
            assert_eq!(found.len(), usize::from(populated));
            if populated {
                assert!(body.windows(2).any(|bytes| bytes == b"\xc2\x85"));
            }
        }
    }
    #[test]
    fn authenticated_http_checks_refresh_and_reject_redirects_without_following() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut received = Vec::new();
            // Two responses on one socket prove that the shared agent reuses it.
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            for sha in ["a", "b"] {
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                received.push(String::from_utf8(request).unwrap());
                let body = response(&[format!(
                    "{} refs/heads/topic/slash\0multi_ack\n",
                    sha.repeat(40)
                )]);
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
                socket.write_all(&body).unwrap();
            }
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            received.push(String::from_utf8(request).unwrap());
            write!(socket, "HTTP/1.1 302 Found\r\nLocation: http://{address}/must-not-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            received
        });
        let mut remote = Remote::parse(
            "origin",
            &format!("http://{address}/team/repo"),
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("http://{address}/repos/team/repo/branches");
        let cancel = AtomicBool::new(false);
        for sha in ["a", "b"] {
            let found = read(&remote, Some("private-test-token"), &names(), &cancel).unwrap();
            assert_eq!(found[0].sha, sha.repeat(40));
        }
        assert!(read(&remote, Some("private-test-token"), &names(), &cancel).is_none());
        // A repeat during cooldown performs no connection attempt.
        assert!(read(&remote, Some("private-test-token"), &names(), &cancel).is_none());
        for request in server.join().unwrap() {
            assert!(
                request.starts_with("GET /team/repo/info/refs?service=git-upload-pack HTTP/1.1")
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: basic ")
            );
            assert!(!request.contains("private-test-token"));
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(read(&remote, Some("changed-token"), &names(), &cancel).is_none());
    }
}
