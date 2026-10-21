//! User-wide provider endpoints; secrets stay in Windows Credential Manager.
use crate::remote::{self, Configuration, Provider, Remote};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MASK: &str = "********";

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Providers {
    pub gitlab: Option<String>,
    pub github: Option<String>,
}

impl Providers {
    pub fn url(&self, provider: Provider) -> Option<&str> {
        match provider {
            Provider::Gitlab => self.gitlab.as_deref(),
            Provider::Github => self.github.as_deref(),
        }
    }

    pub fn apply(&self, config: &mut Configuration) {
        // A repository's explicit API installation remains authoritative.
        if config.api_url.as_ref().is_some_and(|url| !url.is_empty()) {
            return;
        }
        let provider = match config.provider.as_deref() {
            Some("github") => Some(Provider::Github),
            Some("gitlab") => Some(Provider::Gitlab),
            _ => config.remote().ok().map(|r| r.provider).or_else(|| {
                [Provider::Gitlab, Provider::Github].into_iter().find(|p| {
                    let Some(base) = self.url(*p) else {
                        return false;
                    };
                    let Some(remote) = Remote::parse(&config.name, &config.url, Some(p.key()))
                    else {
                        return false;
                    };
                    url::Url::parse(&remote.endpoint)
                        .ok()
                        .zip(url::Url::parse(base).ok())
                        .is_some_and(|(a, b)| a.host_str() == b.host_str())
                })
            }),
        };
        if let Some(provider) = provider
            && let Some(base) = self.url(provider)
        {
            let Some(transport) = Remote::parse(&config.name, &config.url, Some(provider.key()))
            else {
                return;
            };
            let same_host = url::Url::parse(&transport.endpoint)
                .ok()
                .zip(url::Url::parse(base).ok())
                .is_some_and(|(transport, api)| {
                    transport.host_str() == api.host_str()
                        && (url::Url::parse(&config.url)
                            .ok()
                            .is_none_or(|url| !matches!(url.scheme(), "http" | "https"))
                            || transport.port_or_known_default() == api.port_or_known_default())
                });
            if !same_host {
                return;
            }
            if config.project.is_none()
                && url::Url::parse(&config.url)
                    .ok()
                    .is_some_and(|url| matches!(url.scheme(), "https" | "http"))
                && let Ok(api) = url::Url::parse(base)
            {
                let suffix = match provider {
                    Provider::Github => "/api/v3",
                    Provider::Gitlab => "/api/v4",
                };
                if let Some(prefix) = api
                    .path()
                    .strip_suffix(suffix)
                    .filter(|prefix| !prefix.is_empty())
                    && let Some(project) = transport
                        .project
                        .strip_prefix(&format!("{}/", prefix.trim_matches('/')))
                {
                    config.project = Some(project.into());
                }
            }
            config.provider = Some(provider.key().into());
            config.api_url = Some(base.into());
        }
    }
}

fn file() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("git-rustle/providers.json"))
        .ok_or_else(|| "Cannot locate the user's settings directory".into())
}

pub fn load() -> Result<Providers, String> {
    load_file(&file()?)
}

fn load_file(path: &Path) -> Result<Providers, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut settings: Providers = serde_json::from_slice(&bytes)
                .map_err(|_| "Cannot parse global provider settings")?;
            for provider in [Provider::Gitlab, Provider::Github] {
                if let Some(value) = settings.url(provider) {
                    let base = remote::api_base(provider, value)?;
                    match provider {
                        Provider::Gitlab => settings.gitlab = Some(base),
                        Provider::Github => settings.github = Some(base),
                    }
                }
            }
            Ok(settings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Providers::default()),
        Err(_) => Err("Cannot read global provider settings".into()),
    }
}

pub fn default_url(provider: Provider) -> &'static str {
    match provider {
        Provider::Gitlab => "https://gitlab.com/api/v4",
        Provider::Github => "https://api.github.com",
    }
}

#[derive(Clone, Default)]
pub struct Draft {
    pub url: String,
    pub token: String,
    pub saved_url: String,
    pub has_token: bool,
    pub forget: bool,
}

impl Draft {
    pub fn load(settings: &Providers, provider: Provider) -> Result<Self, String> {
        let url = settings
            .url(provider)
            .unwrap_or(default_url(provider))
            .to_string();
        Ok(Self {
            has_token: crate::credentials::read(&url)?
                .or(crate::credentials::legacy_shared(
                    &url,
                    match provider {
                        Provider::Github => "repos",
                        Provider::Gitlab => "projects",
                    },
                )?)
                .is_some(),
            saved_url: url.clone(),
            url,
            ..Self::default()
        })
    }

    pub fn token_text(&self) -> String {
        if self.has_token && self.token.is_empty() && !self.forget {
            MASK.into()
        } else {
            self.token.clone()
        }
    }

    pub fn set_token_text(&mut self, text: String) {
        // The display mask is a sentinel, never a replacement credential.
        if self.has_token && text == MASK && !self.forget {
            return;
        }
        self.forget = text.is_empty() && self.has_token;
        self.token = text;
    }

    pub fn validated_url(&self, provider: Provider) -> Result<String, String> {
        if self.token.len() > 2560 || self.token.chars().any(char::is_control) {
            return Err("Token is too long or contains control characters".into());
        }
        let base = remote::api_base(provider, &self.url)?;
        if base != self.saved_url && self.has_token && self.token.is_empty() && !self.forget {
            return Err("Enter a token for the new URL, or choose Forget saved token.".into());
        }
        Ok(base)
    }
}

pub fn save(provider: Provider, draft: &Draft) -> Result<String, String> {
    save_file(&file()?, provider, draft, |base| {
        if draft.forget {
            crate::credentials::delete_legacy(
                &draft.saved_url,
                match provider {
                    Provider::Github => "repos",
                    Provider::Gitlab => "projects",
                },
            )?;
            crate::credentials::delete(&draft.saved_url)
        } else if !draft.token.is_empty() {
            crate::credentials::write(base, &draft.token)
        } else {
            Ok(())
        }
    })?;
    Ok("Saved for your Windows user. These settings apply to all projects.".into())
}

fn save_file(
    path: &Path,
    provider: Provider,
    draft: &Draft,
    save_token: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    use std::io::Write;
    let base = draft.validated_url(provider)?;
    std::fs::create_dir_all(path.parent().ok_or("Invalid settings path")?)
        .map_err(|_| "Cannot create user settings directory")?;
    let lock = path.with_extension("json.lock");
    let mut handle = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|_| "Cannot lock global settings; another save may be running")?;
    struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = Guard(lock.clone());
    let mut settings = load_file(path)?;
    match provider {
        Provider::Gitlab => settings.gitlab = Some(base.clone()),
        Provider::Github => settings.github = Some(base.clone()),
    }
    handle
        .write_all(
            &serde_json::to_vec_pretty(&settings).map_err(|_| "Cannot encode global settings")?,
        )
        .and_then(|_| handle.sync_all())
        .map_err(|_| "Cannot write global settings")?;
    drop(handle);
    save_token(&base)?;
    std::fs::rename(lock, path)
        .map_err(|_| "Cannot replace global settings; token changes may have been saved".into())
}

/// Only reuse user-wide secrets within the configured API installation.
pub fn token(remote: &Remote) -> Result<Option<String>, String> {
    token_with(
        remote,
        crate::credentials::read,
        crate::credentials::legacy_shared,
    )
}

fn token_with(
    remote: &Remote,
    read: impl FnOnce(&str) -> Result<Option<String>, String>,
    legacy: impl FnOnce(&str, &str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    let Some(base) = credential_base(remote) else {
        return Ok(None);
    };
    if let Some(token) = read(base)? {
        return Ok(Some(token));
    }
    legacy(
        base,
        match remote.provider {
            Provider::Github => "repos",
            Provider::Gitlab => "projects",
        },
    )
}

pub fn credential_base(remote: &Remote) -> Option<&str> {
    remote
        .endpoint
        .rsplit_once(match remote.provider {
            Provider::Github => "/repos/",
            Provider::Gitlab => "/projects/",
        })
        .map(|(base, _)| base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_store_shares_tokens_by_api_installation_and_prefers_explicit_global_account() {
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/one.git",
            Some("gitlab"),
        )
        .unwrap();
        for project in ["team%2Fone", "team%2Ftwo"] {
            remote.endpoint = format!(
                "https://example.invalid/gitlab/api/v4/projects/{project}/repository/branches"
            );
            let token = token_with(
                &remote,
                |base| {
                    assert_eq!(base, "https://example.invalid/gitlab/api/v4");
                    Ok(Some("fake-global".into()))
                },
                |_, _| panic!("explicit account must bypass legacy lookup"),
            )
            .unwrap();
            assert_eq!(token.as_deref(), Some("fake-global"));
            let token = token_with(
                &remote,
                |_| Ok(None),
                |base, segment| {
                    assert_eq!(base, "https://example.invalid/gitlab/api/v4");
                    assert_eq!(segment, "projects");
                    Ok(Some("fake-legacy".into()))
                },
            )
            .unwrap();
            assert_eq!(token.as_deref(), Some("fake-legacy"));
        }
        remote.endpoint =
            "https://other.invalid/api/v4/projects/team%2Fone/repository/branches".into();
        assert!(
            token_with(
                &remote,
                |base| Ok((base == "https://example.invalid/gitlab/api/v4")
                    .then(|| "wrong-server".into())),
                |_, _| Ok(None)
            )
            .unwrap()
            .is_none()
        );
        assert!(
            token_with(
                &remote,
                |_| Err("credential access denied".into()),
                |_, _| Ok(None)
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_project_token_is_shared_across_clones_without_crossing_installations() {
        let dir = tempfile::tempdir().unwrap();
        let base = format!(
            "https://example.invalid/{}/api/v4",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        let a = format!("{base}/projects/team%2Fone/repository/branches");
        let b = format!("{base}/projects/team%2Ftwo/repository/branches");
        struct Cleanup(Vec<String>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for key in &self.0 {
                    let _ = crate::credentials::delete(key);
                }
            }
        }
        let _cleanup = Cleanup(vec![a.clone(), b.clone(), base.clone()]);
        if !crate::credentials::write_fixture(&a, "synthetic-legacy-shared-token") {
            return;
        }
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/two.git",
            Some("gitlab"),
        )
        .unwrap();
        remote.endpoint = b.clone();
        assert_eq!(
            token(&remote).unwrap().as_deref(),
            Some("synthetic-legacy-shared-token")
        );
        remote.endpoint = format!("{base}/other/projects/team%2Ftwo/repository/branches");
        assert!(token(&remote).unwrap().is_none());
        remote.endpoint = b.clone();
        crate::credentials::write(&b, "different-account").unwrap();
        assert!(
            token(&remote).unwrap().is_none(),
            "ambiguous legacy accounts must not be shared"
        );
        crate::credentials::write(&base, "explicit-global-token").unwrap();
        assert_eq!(
            token(&remote).unwrap().as_deref(),
            Some("explicit-global-token")
        );
        crate::credentials::delete_legacy(&base, "projects").unwrap();
        assert!(crate::credentials::read(&a).unwrap().is_none());
        assert!(crate::credentials::read(&b).unwrap().is_none());
    }

    #[test]
    fn global_provider_does_not_redirect_an_unrelated_server_or_explicit_installation() {
        let providers = Providers {
            gitlab: Some("https://company.example/gitlab/api/v4".into()),
            ..Default::default()
        };
        let mut public = Configuration {
            name: "origin".into(),
            url: "https://gitlab.com/team/project.git".into(),
            ..Default::default()
        };
        providers.apply(&mut public);
        assert!(
            public
                .remote()
                .unwrap()
                .endpoint
                .starts_with("https://gitlab.com/api/v4/")
        );
        let mut explicit = Configuration {
            name: "origin".into(),
            url: "https://company.example/team/project.git".into(),
            provider: Some("gitlab".into()),
            api_url: Some("https://company.example/second/api/v4".into()),
            ..Default::default()
        };
        providers.apply(&mut explicit);
        assert_eq!(
            explicit.api_url.as_deref(),
            Some("https://company.example/second/api/v4")
        );
        let mut prefixed = Configuration {
            name: "origin".into(),
            url: "https://company.example/gitlab/team/project.git".into(),
            ..Default::default()
        };
        providers.apply(&mut prefixed);
        assert_eq!(prefixed.remote().unwrap().project, "team/project");
        let mut other_port = Configuration {
            name: "origin".into(),
            url: "https://company.example:8443/team/project.git".into(),
            provider: Some("gitlab".into()),
            ..Default::default()
        };
        providers.apply(&mut other_port);
        assert!(other_port.api_url.is_none());
    }

    #[test]
    fn saved_token_is_shared_by_projects_but_scoped_to_the_api_installation() {
        let dir = tempfile::tempdir().unwrap();
        let base = format!(
            "https://example.invalid/{}/api/v4",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = crate::credentials::delete(&self.0);
            }
        }
        let _cleanup = Cleanup(base.clone());
        if !crate::credentials::write_fixture(&base, "synthetic-global-token") {
            return;
        }
        let providers = Providers {
            gitlab: Some(base.clone()),
            ..Default::default()
        };
        let draft = Draft::load(&providers, Provider::Gitlab).unwrap();
        assert_eq!(draft.token_text(), MASK);
        assert!(draft.token.is_empty());
        for project in ["team/one", "team/two"] {
            let config = Configuration {
                name: "origin".into(),
                url: format!("git@example.invalid:{project}.git"),
                provider: Some("gitlab".into()),
                api_url: Some(base.clone()),
                ..Default::default()
            };
            assert_eq!(
                token(&config.remote().unwrap()).unwrap().as_deref(),
                Some("synthetic-global-token")
            );
        }
        let mut other =
            Remote::parse("origin", "git@other.invalid:team/one.git", Some("gitlab")).unwrap();
        assert_eq!(token(&other).unwrap(), None);
        other.endpoint = format!("{base}/other/projects/team%2Fone/repository/branches");
        assert_eq!(token(&other).unwrap(), None);
    }

    #[test]
    fn global_urls_apply_across_projects_and_keep_provider_settings_separate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        for (provider, url) in [
            (Provider::Gitlab, "https://code.example/gitlab"),
            (Provider::Github, "https://hub.example"),
        ] {
            let draft = Draft {
                url: url.into(),
                ..Default::default()
            };
            save_file(&path, provider, &draft, |_| Ok(())).unwrap();
        }
        let settings = load_file(&path).unwrap();
        for project in ["team/one", "team/two"] {
            let mut config = Configuration {
                name: "origin".into(),
                url: format!("git@code.example:{project}.git"),
                ..Default::default()
            };
            settings.apply(&mut config);
            assert!(
                config
                    .remote()
                    .unwrap()
                    .endpoint
                    .starts_with("https://code.example/gitlab/api/v4/projects/")
            );
        }
        assert_eq!(
            settings.github.as_deref(),
            Some("https://hub.example/api/v3")
        );
    }

    #[test]
    fn masks_preserve_secrets_and_host_changes_require_explicit_credentials() {
        let mut draft = Draft {
            url: default_url(Provider::Github).into(),
            saved_url: default_url(Provider::Github).into(),
            has_token: true,
            ..Default::default()
        };
        assert_eq!(draft.token_text(), MASK);
        draft.set_token_text(MASK.into());
        assert!(draft.token.is_empty());
        draft.url = "https://other.example".into();
        assert!(draft.validated_url(Provider::Github).is_err());
        draft.set_token_text("replacement".into());
        assert!(draft.validated_url(Provider::Github).is_ok());
        draft.set_token_text(String::new());
        assert!(draft.forget);
    }

    #[test]
    fn failed_or_locked_save_keeps_existing_global_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        let draft = Draft {
            url: default_url(Provider::Github).into(),
            ..Default::default()
        };
        save_file(&path, Provider::Github, &draft, |_| Ok(())).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(save_file(&path, Provider::Gitlab, &draft, |_| Err("failed".into())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::write(path.with_extension("json.lock"), b"locked").unwrap();
        assert!(save_file(&path, Provider::Github, &draft, |_| panic!()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
