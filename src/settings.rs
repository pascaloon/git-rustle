#![allow(dead_code)] // Legacy adapters remain as regression-test references.
//! Lazy modal settings state and worker-side persistence.
use crate::remote::{self, Configuration, Provider};
use crate::term::{KeyCode, KeyEvent};
use std::path::Path;

impl Provider {
    pub fn key(self) -> &'static str {
        match self {
            Self::Gitlab => "gitlab",
            Self::Github => "github",
        }
    }
}

#[derive(Clone, Default)]
pub struct Draft {
    pub config: Configuration,
    pub token: String,
    pub forget: bool,
    pub cursor: usize,
}

impl Draft {
    fn new(config: &Configuration, provider: Provider) -> Self {
        let mut config = config.clone();
        let same = config.remote().ok().is_none_or(|r| r.provider == provider);
        config.provider = Some(provider.key().into());
        if !same {
            config.api_url = None;
        }
        if config.api_url.is_none() {
            let remote = remote::Remote::parse(&config.name, &config.url, Some(provider.key()));
            config.api_url = remote.as_ref().map(|r| match provider {
                Provider::Gitlab => r.endpoint.split("/projects/").next().unwrap().to_string(),
                Provider::Github => r.endpoint.split("/repos/").next().unwrap().to_string(),
            });
            if !same || config.api_url.is_none() {
                config.api_url = Some(
                    match provider {
                        Provider::Gitlab => "https://gitlab.com/api/v4",
                        Provider::Github => "https://api.github.com",
                    }
                    .into(),
                );
            }
        }
        if config.project.is_none() {
            config.project =
                remote::Remote::parse(&config.name, &config.url, Some("gitlab")).map(|r| r.project);
        }
        Self {
            config,
            ..Self::default()
        }
    }

    pub fn validated(&self) -> Result<Configuration, String> {
        let mut config = self.config.clone();
        let provider = if config.provider.as_deref() == Some("github") {
            Provider::Github
        } else {
            Provider::Gitlab
        };
        config.api_url = Some(remote::api_base(
            provider,
            config.api_url.as_deref().unwrap_or_default(),
        )?);
        let project = config.project.as_deref().unwrap_or_default().trim();
        if project.is_empty() {
            return Err("Enter a project path such as group/subgroup/project".into());
        }
        config.project = Some(project.to_string());
        config.remote()?;
        if self.token.len() > 2560 || self.token.chars().any(char::is_control) {
            return Err("Token is too long or contains control characters".into());
        }
        Ok(config)
    }
}

pub enum Request {
    Load,
    Test(Draft),
    Save(Draft),
    SaveProject(Configuration),
    SaveProvider(Provider, crate::provider_settings::Draft),
}
pub enum Outcome {
    Loaded(Result<Vec<Configuration>, String>),
    Tested(Result<String, String>),
    Saved(Result<String, String>),
    LoadedAll(Result<Loaded, String>),
    SavedPage(Result<String, String>),
}

pub struct Loaded {
    configs: Vec<Configuration>,
    providers: [crate::provider_settings::Draft; 2],
}

#[derive(Default)]
pub struct Settings {
    pub page: usize,
    pub project_selected: usize,
    pub projects: Vec<Configuration>,
    pub providers: [crate::provider_settings::Draft; 2],
    pub open: bool,
    pub session: u32,
    pub tab: usize,
    pub field: usize,
    pub selected: [usize; 2],
    pub drafts: [Vec<Draft>; 2],
    pub busy: bool,
    pub saving: bool,
    pub status: String,
    pub error: bool,
    pub request: Option<Request>,
}

impl Settings {
    pub fn show(&mut self) {
        self.session = self.session.wrapping_add(1);
        self.open = true;
        self.busy = true;
        self.saving = false;
        self.field = 0;
        self.drafts = Default::default();
        self.projects.clear();
        self.providers = Default::default();
        self.status = "Loading repository remotes…".into();
        self.error = false;
        self.request = Some(Request::Load);
    }

    pub fn current(&self) -> Option<&Draft> {
        self.drafts[self.tab].get(self.selected[self.tab])
    }
    fn current_mut(&mut self) -> Option<&mut Draft> {
        self.drafts[self.tab].get_mut(self.selected[self.tab])
    }

    pub fn finish(&mut self, outcome: Outcome) {
        self.busy = false;
        self.saving = false;
        match outcome {
            Outcome::LoadedAll(Ok(loaded)) => {
                self.projects = loaded.configs.clone();
                self.project_selected = self
                    .project_selected
                    .min(self.projects.len().saturating_sub(1));
                self.providers = loaded.providers;
                self.finish(Outcome::Loaded(Ok(loaded.configs)));
                self.status = "Project settings apply here. GitLab and GitHub settings apply to all projects.".into();
            }
            Outcome::SavedPage(Ok(message)) => {
                if self.page > 0 {
                    let draft = &mut self.providers[self.page - 1];
                    draft.has_token = !draft.forget && (!draft.token.is_empty() || draft.has_token);
                    draft.saved_url = draft
                        .validated_url(if self.page == 1 {
                            Provider::Gitlab
                        } else {
                            Provider::Github
                        })
                        .unwrap_or_else(|_| draft.url.clone());
                    draft.token.clear();
                    draft.forget = false;
                }
                self.status = message;
                self.error = false;
            }
            Outcome::LoadedAll(Err(error)) | Outcome::SavedPage(Err(error)) => {
                self.status = error;
                self.error = true;
            }
            Outcome::Loaded(Ok(configs)) => {
                for (tab, provider) in [Provider::Gitlab, Provider::Github].into_iter().enumerate()
                {
                    self.selected[tab] = configs
                        .iter()
                        .position(|c| c.remote().is_ok_and(|r| r.provider == provider))
                        .unwrap_or(0);
                    self.drafts[tab] = configs.iter().map(|c| Draft::new(c, provider)).collect();
                }
                self.status = if configs.is_empty() {
                    "No remotes configured. Add one with git remote add, then reopen settings."
                } else {
                    "Choose a remote. Test checks these values without saving or fetching."
                }
                .into();
                self.error = false;
            }
            Outcome::Loaded(Err(error))
            | Outcome::Tested(Err(error))
            | Outcome::Saved(Err(error)) => {
                self.status = error;
                self.error = true;
            }
            Outcome::Tested(Ok(message)) => {
                self.status = message;
                self.error = false;
            }
            Outcome::Saved(Ok(message)) => {
                if let Some(draft) = self.current_mut() {
                    if let Ok(config) = draft.validated() {
                        draft.config = config;
                    }
                    draft.token.clear();
                    draft.forget = false;
                    draft.cursor = 0;
                }
                // A remote has one provider. Discard an outdated draft on the other tab.
                if let Some(saved) = self.current().map(|d| d.config.clone()) {
                    let other = 1 - self.tab;
                    let provider = if other == 0 {
                        Provider::Gitlab
                    } else {
                        Provider::Github
                    };
                    for draft in &mut self.drafts[other] {
                        if draft.config.name == saved.name {
                            *draft = Draft::new(&saved, provider);
                        }
                    }
                }
                self.status = message;
                self.error = false;
            }
        }
    }

    pub fn save_page(&mut self) {
        if self.busy {
            return;
        }
        self.request = if self.page == 0 {
            self.projects
                .get(self.project_selected)
                .cloned()
                .map(Request::SaveProject)
        } else {
            let provider = if self.page == 1 {
                Provider::Gitlab
            } else {
                Provider::Github
            };
            let draft = self.providers[self.page - 1].clone();
            if let Err(error) = draft.validated_url(provider) {
                self.status = error;
                self.error = true;
                return;
            }
            Some(Request::SaveProvider(provider, draft))
        };
        if self.request.is_some() {
            self.busy = true;
            self.saving = true;
            self.error = false;
            self.status = "Saving settings…".into();
        }
    }

    pub fn action(&mut self, save: bool) {
        if self.busy {
            return;
        }
        let Some(draft) = self.current() else { return };
        match draft.validated() {
            Err(error) => {
                self.status = error;
                self.error = true;
            }
            Ok(config) => {
                let mut draft = draft.clone();
                draft.config = config;
                self.request = Some(if save {
                    Request::Save(draft)
                } else {
                    Request::Test(draft)
                });
                self.busy = true;
                self.saving = save;
                self.error = false;
                self.status = if save {
                    "Saving settings…"
                } else {
                    "Testing API connection… (Esc closes; graph stays responsive)"
                }
                .into();
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc && !self.saving {
            self.open = false;
            self.request = None;
            self.drafts = Default::default();
            return;
        }
        if self.busy {
            return;
        }
        match (key.code, key.ctrl) {
            (KeyCode::F(1), _) => self.tab = 0,
            (KeyCode::F(2), _) => self.tab = 1,
            (KeyCode::Left | KeyCode::Right, true) => self.tab = 1 - self.tab,
            (KeyCode::Left | KeyCode::Right, false) if self.field == 0 => self.tab = 1 - self.tab,
            (KeyCode::Left | KeyCode::Right, false) if self.field == 1 => {
                let n = self.drafts[self.tab].len();
                if n > 0 {
                    self.selected[self.tab] = (self.selected[self.tab]
                        + if key.code == KeyCode::Left { n - 1 } else { 1 })
                        % n;
                }
            }
            (KeyCode::Tab, _) => {
                self.field = (self.field + if key.shift { 6 } else { 1 }) % 7;
                self.end_cursor();
            }
            (KeyCode::Up, _) => {
                self.field = (self.field + 6) % 7;
                self.end_cursor();
            }
            (KeyCode::Down, _) => {
                self.field = (self.field + 1) % 7;
                self.end_cursor();
            }
            (KeyCode::Char('s'), true) => self.action(true),
            (KeyCode::Char('t'), true) => self.action(false),
            (KeyCode::Enter, _) if self.field == 5 => self.action(false),
            (KeyCode::Enter, _) if self.field == 6 => self.action(true),
            (KeyCode::Enter, _) => {
                self.field = (self.field + 1) % 7;
                self.end_cursor();
            }
            (KeyCode::Char('d'), true) if self.field == 4 => {
                if let Some(draft) = self.current_mut() {
                    draft.token.clear();
                    draft.cursor = 0;
                    draft.forget = !draft.forget;
                }
            }
            _ if (2..=4).contains(&self.field) => self.edit(key),
            _ => {}
        }
    }

    pub fn end_cursor(&mut self) {
        let field = self.field;
        if let Some(draft) = self.current_mut() {
            draft.cursor = match field {
                2 => draft.config.api_url.as_deref().unwrap_or_default(),
                3 => draft.config.project.as_deref().unwrap_or_default(),
                _ => &draft.token,
            }
            .len();
        }
    }

    fn edit(&mut self, key: KeyEvent) {
        let field = self.field;
        let Some(draft) = self.current_mut() else {
            return;
        };
        let text = match field {
            2 => draft.config.api_url.get_or_insert_default(),
            3 => draft.config.project.get_or_insert_default(),
            _ => &mut draft.token,
        };
        let mut cursor = draft.cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let prev = text[..cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        let next = text[cursor..]
            .chars()
            .next()
            .map_or(cursor, |c| cursor + c.len_utf8());
        match (key.code, key.ctrl) {
            (KeyCode::Char('u'), true) => {
                text.clear();
                cursor = 0;
            }
            (KeyCode::Left, _) => cursor = prev,
            (KeyCode::Right, _) => cursor = next,
            (KeyCode::Home, _) => cursor = 0,
            (KeyCode::End, _) => cursor = text.len(),
            (KeyCode::Backspace, _) => {
                text.replace_range(prev..cursor, "");
                cursor = prev;
            }
            (KeyCode::Delete, _) => {
                text.replace_range(cursor..next, "");
            }
            (KeyCode::Char(c), false)
                if !key.alt && !c.is_control() && text.len() + c.len_utf8() <= 2560 =>
            {
                text.insert(cursor, c);
                cursor += c.len_utf8();
            }
            _ => {}
        }
        draft.cursor = cursor;
        if field == 4 && !text.is_empty() {
            draft.forget = false;
        }
    }
}

pub fn execute(path: &Path, request: Request) -> Outcome {
    match request {
        Request::Load => Outcome::LoadedAll((|| {
            let settings = crate::provider_settings::load()?;
            Ok(Loaded {
                configs: remote::configurations(path)?,
                providers: [
                    crate::provider_settings::Draft::load(&settings, Provider::Gitlab)?,
                    crate::provider_settings::Draft::load(&settings, Provider::Github)?,
                ],
            })
        })()),
        Request::SaveProvider(provider, draft) => {
            Outcome::SavedPage(crate::provider_settings::save(provider, &draft))
        }
        Request::SaveProject(mut config) => Outcome::SavedPage((|| {
            config.project = config
                .project
                .filter(|p| !p.trim().is_empty())
                .map(|p| p.trim().to_string());
            let mut resolved = config.clone();
            crate::provider_settings::load()?.apply(&mut resolved);
            resolved.remote()?;
            save_config(path, &config, || Ok(()))?;
            Ok("Saved for this project. Remote branches will refresh automatically.".into())
        })()),
        Request::Test(draft) => Outcome::Tested(draft.validated().and_then(|config| {
            let remote = config.remote()?;
            remote::test_connection(
                path,
                &remote,
                (!draft.token.is_empty()).then_some(draft.token.as_str()),
                draft.forget,
            )
        })),
        Request::Save(draft) => Outcome::Saved(save(path, &draft)),
    }
}

pub fn save(path: &Path, draft: &Draft) -> Result<String, String> {
    let config = draft.validated()?;
    let remote = config.remote()?;
    save_config(path, &config, || {
        if draft.forget {
            crate::credentials::delete(&remote.endpoint)
        } else if !draft.token.is_empty() {
            crate::credentials::write(&remote.endpoint, &draft.token)
        } else {
            Ok(())
        }
    })?;
    Ok("Saved for this repository. Remote branches will refresh automatically.".into())
}

fn save_config(
    path: &Path,
    config: &Configuration,
    save_token: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    use crate::git::command;
    use std::io::Write;
    let worktree = command::checked(
        path,
        &["config", "--bool", "--get", "extensions.worktreeConfig"],
    )
    .is_ok_and(|v| v.starts_with(b"true"));
    let raw = command::checked(
        path,
        &[
            "rev-parse",
            "--git-path",
            if worktree {
                "config.worktree"
            } else {
                "config"
            },
        ],
    )?;
    let file = path.join(String::from_utf8_lossy(&raw).trim());
    let lock = std::path::PathBuf::from(format!("{}.lock", file.display()));
    let mut handle = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|_| {
            "Cannot lock repository config. Another Git operation may be running.".to_string()
        })?;
    struct Guard(std::path::PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = Guard(lock.clone());
    let current = remote::configurations(path)?;
    if !current
        .iter()
        .any(|c| c.name == config.name && c.url == config.url)
    {
        return Err("Remote changed while settings were open; close and reopen settings.".into());
    }
    let original = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(e) if worktree && e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return Err("Cannot read repository config".into()),
    };
    handle
        .write_all(&original)
        .map_err(|_| "Cannot prepare updated config")?;
    drop(handle);
    for (key, value) in [
        ("rustle-provider", config.provider.as_deref()),
        ("rustle-api-url", config.api_url.as_deref()),
        ("rustle-project", config.project.as_deref()),
    ] {
        command::checked(
            path,
            &[
                "config",
                "--file",
                lock.to_str().ok_or("Config path is not UTF-8")?,
                "--replace-all",
                &format!("remote.{}.{key}", config.name),
                value.unwrap_or_default(),
            ],
        )?;
    }
    save_token()?;
    std::fs::rename(&lock, &file).map_err(|_| "Cannot replace repository config; original config retained, but token changes may have been saved".to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::command::checked;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }
    fn config() -> Configuration {
        Configuration {
            name: "origin".into(),
            url: "git@work-alias:team/project.git".into(),
            provider: Some("gitlab".into()),
            api_url: Some("https://code.example:8443/gitlab".into()),
            project: Some("team/subgroup/project".into()),
        }
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        checked(dir.path(), &["init", "--quiet"]).unwrap();
        checked(dir.path(), &["remote", "add", "origin", &config().url]).unwrap();
        checked(
            dir.path(),
            &[
                "config",
                "remote.origin.pushurl",
                "git@push-alias:team/project.git",
            ],
        )
        .unwrap();
        dir
    }

    #[test]
    fn modal_edits_unicode_and_keeps_independent_tabs() {
        let mut app = crate::app::App::new();
        app.handle_key(key(KeyCode::Char('s')));
        assert!(app.settings.open);
        app.settings.finish(Outcome::Loaded(Ok(vec![config()])));
        app.settings.field = 3;
        app.settings.end_cursor();
        app.handle_key(key(KeyCode::Char('é')));
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Delete));
        assert_eq!(
            app.settings.current().unwrap().config.project.as_deref(),
            Some("team/subgroup/project")
        );
        app.handle_key(key(KeyCode::F(2)));
        assert_eq!(app.settings.tab, 1);
        app.handle_key(key(KeyCode::F(1)));
        app.settings.field = 4;
        app.settings.end_cursor();
        app.handle_key(key(KeyCode::Char('q')));
        assert!(!app.quit);
        assert_eq!(app.settings.current().unwrap().token, "q");
        let session = app.settings.session;
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.settings.open);
        assert!(app.settings.drafts.iter().all(Vec::is_empty));
        app.handle_key(key(KeyCode::Char('s')));
        assert_ne!(app.settings.session, session);
        app.settings.saving = true;
        app.settings.busy = true;
        app.handle_key(key(KeyCode::Esc));
        assert!(app.settings.open);
    }

    #[test]
    fn saves_on_premise_config_without_changing_git_transport() {
        let dir = fixture();
        let draft = Draft::new(&config(), Provider::Gitlab);
        save(dir.path(), &draft).unwrap();
        let configs = remote::configurations(dir.path()).unwrap();
        assert_eq!(configs[0].url, config().url);
        assert_eq!(
            configs[0].remote().unwrap().endpoint,
            "https://code.example:8443/gitlab/api/v4/projects/team%2Fsubgroup%2Fproject/repository/branches"
        );
        assert_eq!(
            checked(dir.path(), &["config", "remote.origin.pushurl"]).unwrap(),
            b"git@push-alias:team/project.git\n"
        );
        assert!(!dir.path().join(".git/config.lock").exists());
    }

    #[test]
    fn locked_stale_or_failed_save_preserves_config_and_credentials() {
        let dir = fixture();
        let file = dir.path().join(".git/config");
        let lock = dir.path().join(".git/config.lock");
        let original = std::fs::read(&file).unwrap();
        std::fs::write(&lock, b"other git process").unwrap();
        assert!(
            save_config(dir.path(), &config(), || panic!(
                "must not touch credentials"
            ))
            .is_err()
        );
        assert_eq!(std::fs::read(&lock).unwrap(), b"other git process");
        std::fs::remove_file(lock).unwrap();
        let mut stale = config();
        stale.url.push_str("-changed");
        assert!(save_config(dir.path(), &stale, || panic!("must not touch credentials")).is_err());
        assert!(
            save_config(dir.path(), &config(), || Err(
                "credential store unavailable".into()
            ))
            .is_err()
        );
        assert_eq!(std::fs::read(file).unwrap(), original);
        assert!(!dir.path().join(".git/config.lock").exists());
    }

    #[test]
    fn worktree_configuration_is_saved_to_its_own_file() {
        let dir = fixture();
        checked(dir.path(), &["config", "extensions.worktreeConfig", "true"]).unwrap();
        let original = std::fs::read(dir.path().join(".git/config")).unwrap();
        save(dir.path(), &Draft::new(&config(), Provider::Gitlab)).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(".git/config")).unwrap(),
            original
        );
        assert!(dir.path().join(".git/config.worktree").exists());
        assert_eq!(
            remote::configurations(dir.path()).unwrap()[0]
                .project
                .as_deref(),
            Some("team/subgroup/project")
        );
    }

    #[test]
    fn project_save_keeps_transport_and_existing_api_override() {
        let dir = fixture();
        let mut config = config();
        config.provider = Some("gitlab".into());
        config.project = Some("team/renamed".into());
        match execute(dir.path(), Request::SaveProject(config)) {
            Outcome::SavedPage(Ok(_)) => {}
            _ => panic!("project save failed"),
        }
        let configs = remote::configurations(dir.path()).unwrap();
        assert_eq!(configs[0].url, self::config().url);
        assert_eq!(configs[0].api_url, self::config().api_url);
        assert_eq!(configs[0].project.as_deref(), Some("team/renamed"));
        assert!(configs[0].remote().is_ok());
    }

    #[test]
    fn global_drafts_can_save_without_project_remotes_and_remain_independent() {
        let mut settings = Settings {
            page: 1,
            ..Default::default()
        };
        settings.providers[0].url = "https://gitlab.com".into();
        settings.providers[1].url = "https://github.com".into();
        settings.providers[0].token = "new-token".into();
        settings.save_page();
        assert!(matches!(
            settings.request,
            Some(Request::SaveProvider(Provider::Gitlab, _))
        ));
        settings.finish(Outcome::SavedPage(Ok("saved".into())));
        assert!(settings.providers[0].has_token);
        assert!(settings.providers[0].token.is_empty());
        assert_eq!(
            settings.providers[0].token_text(),
            crate::provider_settings::MASK
        );
        assert!(!settings.providers[1].has_token);
        assert_eq!(settings.providers[1].url, "https://github.com");
    }
}
