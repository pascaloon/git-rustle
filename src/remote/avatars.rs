//! Public, credential-free avatar bytes shared by all repositories of this OS user.
use super::{Provider, Remote};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const MAX_IMAGE: u64 = 256 * 1024;
const TTL: u64 = 86400;

pub(crate) struct Cache {
    directory: PathBuf,
}

impl Cache {
    pub fn new(git_dir: &Path) -> Option<Self> {
        // Only the application cache directory is reused, never its repository key.
        Some(Self::at(
            crate::cache::cache_path(git_dir)?
                .parent()?
                .join("avatars-v1"),
        ))
    }

    pub fn at(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn file(&self, category: &str, key: &str) -> PathBuf {
        let hash =
            gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, key.as_bytes())
                .expect("SHA-1 is enabled");
        self.directory.join(format!("{category}-{hash}.bin"))
    }

    fn read(&self, category: &str, key: &str, limit: u64) -> Option<Vec<u8>> {
        let file = fs::File::open(self.file(category, key)).ok()?;
        let meta = file.metadata().ok()?;
        if meta.len() > limit || meta.modified().ok()?.elapsed().ok()?.as_secs() >= TTL {
            return None;
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.take(limit + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 <= limit).then_some(bytes)
    }

    fn write(&self, category: &str, key: &str, bytes: &[u8]) -> bool {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = self.file(category, key);
        let tmp = path.with_extension(format!(
            "{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(&self.directory)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(bytes)?;
            drop(file);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result.is_ok()
    }

    pub fn image(&self, url: &str) -> Option<Vec<u8>> {
        let bytes = self.read("image", url, MAX_IMAGE)?;
        image_format(&bytes)?;
        Some(bytes)
    }

    pub fn store_image(&self, url: &str, bytes: &[u8]) -> bool {
        bytes.len() as u64 <= MAX_IMAGE
            && image_format(bytes).is_some()
            && self.write("image", url, bytes)
    }

    fn identity_key(remote: &Remote, email: &str) -> Option<String> {
        if email.is_empty() {
            return None;
        }
        let (instance, _) = remote.endpoint.rsplit_once(match remote.provider {
            Provider::Github => "/repos/",
            Provider::Gitlab => "/projects/",
        })?;
        // Equal emails on separate providers/installations must never share a binding.
        Some(format!("{}\0{instance}\0{email}", remote.provider.key()))
    }

    pub fn identity(&self, remote: &Remote, email: &str) -> Option<String> {
        let key = Self::identity_key(remote, email)?;
        let url = String::from_utf8(self.read("author", &key, 16384)?).ok()?;
        super::requests::safe_image_url(&url).then_some(url)
    }

    pub fn store_identity(&self, remote: &Remote, email: &str, url: &str) {
        if let Some(key) = Self::identity_key(remote, email)
            && url.len() <= 16384
            && super::requests::safe_image_url(url)
        {
            self.write("author", &key, url.as_bytes());
        }
    }
}

pub fn image_format(bytes: &[u8]) -> Option<gpui::ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(gpui::ImageFormat::Png)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(gpui::ImageFormat::Jpeg)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(gpui::ImageFormat::Webp)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHOTO: &[u8] = b"\x89PNG\r\n\x1a\nfixture";
    const URL: &str = "https://avatars.example/u/42?s=64";

    #[test]
    fn binary_images_are_shared_and_partial_stale_or_oversized_files_are_misses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().into());
        assert!(cache.store_image(URL, PHOTO));
        let path = cache.file("image", URL);
        assert_eq!(fs::read(&path).unwrap(), PHOTO); // no JSON, header or base64
        assert_eq!(Cache::at(dir.path().into()).image(URL).unwrap(), PHOTO);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        fs::write(&path, b"incomplete").unwrap();
        assert!(cache.image(URL).is_none());
        fs::write(&path, vec![0; MAX_IMAGE as usize + 1]).unwrap();
        assert!(cache.image(URL).is_none());
        assert!(cache.store_image(URL, PHOTO));
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        assert!(cache.image(URL).is_none());
    }

    #[test]
    fn author_bindings_share_repositories_but_isolate_provider_instances() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().into());
        let mut remote = Remote::parse("origin", "https://github.com/a/one.git", None).unwrap();
        cache.store_identity(&remote, "alice@example.com", URL);
        remote.endpoint = "https://api.github.com/repos/b/two/branches".into();
        assert_eq!(
            cache.identity(&remote, "alice@example.com").as_deref(),
            Some(URL)
        );
        assert!(cache.identity(&remote, "bob@example.com").is_none());
        remote.endpoint = "https://enterprise.example/api/v3/repos/a/one/branches".into();
        assert!(cache.identity(&remote, "alice@example.com").is_none());
        remote.endpoint = "https://enterprise.example/repos/one/api/v3/repos/a/one/branches".into();
        cache.store_identity(&remote, "alice@example.com", URL);
        remote.endpoint = "https://enterprise.example/repos/two/api/v3/repos/a/one/branches".into();
        assert!(cache.identity(&remote, "alice@example.com").is_none());
        let gitlab = Remote::parse("origin", "https://gitlab.com/a/one.git", None).unwrap();
        assert!(cache.identity(&gitlab, "alice@example.com").is_none());
        assert_eq!(
            Cache::new(Path::new("one/.git")).map(|c| c.directory),
            Cache::new(Path::new("two/.git")).map(|c| c.directory)
        );
    }

    #[test]
    fn concurrent_writes_publish_complete_images_without_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let path = dir.path();
                scope.spawn(move || {
                    Cache::at(path.into()).store_image(URL, PHOTO);
                });
            }
        });
        assert_eq!(Cache::at(dir.path().into()).image(URL).unwrap(), PHOTO);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
