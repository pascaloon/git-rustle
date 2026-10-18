//! Public, credential-free avatar bytes shared by all repositories of this OS user.
use super::{Provider, Remote};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

pub(super) const MAX_IMAGE: u64 = 256 * 1024;
const TTL: u64 = 86400;

pub(crate) struct Cache {
    directory: PathBuf,
    control: Arc<Control>,
    generation: u64,
}

#[derive(Default)]
struct Control {
    generation: AtomicU64,
    write: Mutex<()>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stats {
    pub bytes: u64,
    pub images: usize,
}

// The cache is flat. Never traverse directories or links, or delete unrelated
// files. Temporary files have the exact writer-generated numeric suffix.
fn owned_file(name: &str) -> bool {
    let Some(tail) = name
        .strip_prefix("image-")
        .or_else(|| name.strip_prefix("author-"))
    else {
        return false;
    };
    let Some(hash) = tail.get(..40) else {
        return false;
    };
    if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let suffix = &tail[40..];
    suffix == ".bin"
        || suffix
            .strip_prefix('.')
            .and_then(|s| s.strip_suffix(".tmp"))
            .and_then(|s| s.split_once('-'))
            .is_some_and(|(pid, seq)| {
                !pid.is_empty()
                    && !seq.is_empty()
                    && pid.bytes().chain(seq.bytes()).all(|b| b.is_ascii_digit())
            })
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
        static CONTROLS: OnceLock<Mutex<HashMap<PathBuf, Weak<Control>>>> = OnceLock::new();
        let mut controls = CONTROLS.get_or_init(Default::default).lock().unwrap();
        if controls.len() > 256 {
            controls.retain(|_, v| v.strong_count() != 0);
        }
        let control = controls
            .get(&directory)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let control = Arc::new(Control::default());
                controls.insert(directory.clone(), Arc::downgrade(&control));
                control
            });
        let generation = control.generation.load(Ordering::Acquire);
        Self {
            directory,
            control,
            generation,
        }
    }

    fn files(&self) -> std::io::Result<Vec<fs::DirEntry>> {
        match fs::symlink_metadata(&self.directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(std::io::Error::other(
                    "Images cache path is not a regular directory",
                ));
            }
            Ok(_) => {}
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if kind.is_file() && owned_file(&entry.file_name().to_string_lossy()) {
                files.push(entry);
            }
        }
        Ok(files)
    }

    pub fn stats(&self) -> std::io::Result<Stats> {
        let mut stats = Stats::default();
        for file in self.files()? {
            match file.metadata() {
                Ok(meta) => {
                    stats.bytes = stats.bytes.saturating_add(meta.len());
                    let name = file.file_name();
                    let name = name.to_string_lossy();
                    stats.images +=
                        usize::from(name.starts_with("image-") && name.ends_with(".bin"));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(stats)
    }

    pub fn clear(&self) -> std::io::Result<Stats> {
        let _write = self.control.write.lock().unwrap();
        let files = self.files()?;
        fs::create_dir_all(&self.directory)?;
        // Do not resurrect old JSON photos when the binary cache was cleared.
        fs::write(self.directory.join("cleared"), [])?;
        self.control.generation.fetch_add(1, Ordering::AcqRel);
        for file in files {
            match fs::remove_file(file.path()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        self.stats()
    }

    pub fn legacy_allowed(&self) -> bool {
        !self.directory.join("cleared").exists()
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
        let _write = self.control.write.lock().unwrap();
        if self.generation != self.control.generation.load(Ordering::Acquire) {
            return false;
        }
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
    fn clearing_reports_exact_size_preserves_other_files_and_invalidates_old_writers() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().join("photos"));
        assert_eq!(cache.stats().unwrap(), Stats::default());
        assert!(!cache.directory.exists());
        let remote = Remote::parse("origin", "https://github.com/a/one.git", None).unwrap();
        assert!(cache.store_image(URL, PHOTO));
        cache.store_identity(&remote, "alice@example.com", URL);
        assert_eq!(
            cache.stats().unwrap(),
            Stats {
                bytes: (PHOTO.len() + URL.len()) as u64,
                images: 1
            }
        );
        fs::write(cache.directory.join("unrelated.bin"), b"keep").unwrap();
        let nested = cache.directory.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("image-test.bin"), b"keep").unwrap();
        let other = Cache::at(dir.path().join("other"));
        assert_eq!(cache.clear().unwrap(), Stats::default());
        assert_eq!(
            fs::read(cache.directory.join("unrelated.bin")).unwrap(),
            b"keep"
        );
        assert!(nested.join("image-test.bin").exists());
        assert!(cache.image(URL).is_none());
        assert!(cache.identity(&remote, "alice@example.com").is_none());
        assert!(!cache.legacy_allowed());
        assert!(!cache.store_image(URL, PHOTO));
        let fresh = Cache::at(cache.directory.clone());
        assert!(fresh.store_image(URL, PHOTO));
        assert!(other.store_image(URL, PHOTO));
    }

    #[test]
    fn downloads_started_before_clear_cannot_repopulate_it_afterward() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().into());
        let ready = std::sync::Barrier::new(5);
        let cleared = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            for _ in 0..4 {
                let (ready, cleared) = (&ready, &cleared);
                let path = dir.path();
                jobs.push(scope.spawn(move || {
                    let old = Cache::at(path.into());
                    ready.wait();
                    cleared.wait();
                    old.store_image(URL, PHOTO)
                }));
            }
            ready.wait();
            cache.clear().unwrap();
            cleared.wait();
            for job in jobs {
                assert!(!job.join().unwrap());
            }
        });
        assert_eq!(cache.stats().unwrap(), Stats::default());
    }

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
