//! Explicit executable launches; input copies live until the editor's wait process exits.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
type Result<T> = std::result::Result<T, String>;
const LIMIT: u64 = 8 * 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub scanned: bool,
    pub tools: Vec<Entry>,
}
impl Config {
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.tools.iter().find(|entry| entry.id == id)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub executable: PathBuf,
    #[serde(default)]
    pub preset: Preset,
    pub diff_arguments: Option<Vec<String>>,
    pub merge_arguments: Option<Vec<String>>,
}
impl Entry {
    pub fn diff_tool(&self) -> Option<Tool> {
        self.diff_arguments.as_ref().map(|arguments| Tool {
            executable: self.executable.clone(),
            arguments: arguments.clone(),
            preset: self.preset,
        })
    }
    pub fn merge_tool(&self) -> Option<Tool> {
        self.merge_arguments.as_ref().map(|arguments| Tool {
            executable: self.executable.clone(),
            arguments: arguments.clone(),
            preset: self.preset,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tool {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    #[serde(default)]
    pub preset: Preset,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Preset {
    #[default]
    Custom,
    VsCode,
    Araxis,
}

impl Tool {
    fn uses_vscode_cli(&self, merge: bool) -> bool {
        self.preset == Preset::VsCode
            || self.preset == Preset::Custom
                && self
                    .executable
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("Code.exe"))
                && self.arguments.iter().any(|a| a == "--wait")
                && self
                    .arguments
                    .iter()
                    .any(|a| a == if merge { "--merge" } else { "--diff" })
    }
    fn validate_configuration(&self, merge: bool) -> Result<()> {
        if !self.executable.is_absolute()
            || !self
                .executable
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("exe"))
        {
            return Err("Choose an absolute path to an .exe application; command scripts are not supported.".into());
        }
        if self.arguments.len() > 64
            || self
                .arguments
                .iter()
                .any(|a| a.len() > 8192 || a.contains('\0'))
        {
            return Err("Tool arguments are too long.".into());
        }
        let text = self.arguments.join("\n");
        let required: &[&str] = if merge {
            &["{left}", "{right}", "{base}", "{output}"]
        } else {
            &["{left}", "{right}"]
        };
        if required.iter().any(|key| !text.contains(key)) {
            return Err(format!("Arguments must include {}.", required.join(", ")));
        }
        if self.preset == Preset::VsCode && !self.arguments.iter().any(|a| a == "--wait") {
            return Err("VS Code arguments must include --wait.".into());
        }
        if self.preset == Preset::Araxis
            && !self
                .arguments
                .iter()
                .any(|a| a.eq_ignore_ascii_case("/wait"))
        {
            return Err("Araxis arguments must include /wait.".into());
        }
        Ok(())
    }
    pub fn validate(&self, merge: bool) -> Result<()> {
        self.validate_configuration(merge)?;
        if !self.executable.is_file() {
            return Err("The configured application is unavailable. Edit or remove it in Settings → General.".into());
        }
        if self.uses_vscode_cli(merge) {
            vscode_cli(&self.executable)?;
        }
        Ok(())
    }
    fn command(&self, paths: &BTreeMap<&str, PathBuf>, merge: bool) -> Result<Command> {
        self.validate(merge)?;
        let mut command = Command::new(&self.executable);
        if self.uses_vscode_cli(merge) {
            command
                .arg(vscode_cli(&self.executable)?)
                .env("ELECTRON_RUN_AS_NODE", "1")
                .env_remove("VSCODE_DEV");
        }
        for argument in &self.arguments {
            if let Some(path) = paths.get(argument.as_str()) {
                command.arg(path);
                continue;
            }
            // Substitute only in the original argument, never inside an inserted path.
            let mut value = OsString::new();
            let mut rest = argument.as_str();
            while !rest.is_empty() {
                let next = paths
                    .iter()
                    .filter_map(|(key, path)| rest.find(key).map(|at| (at, *key, path)))
                    .min_by_key(|x| x.0);
                if let Some((at, key, path)) = next {
                    value.push(&rest[..at]);
                    value.push(path);
                    rest = &rest[at + key.len()..];
                } else {
                    value.push(rest);
                    break;
                }
            }
            command.arg(value);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        Ok(command)
    }
}

fn vscode_cli(exe: &Path) -> Result<PathBuf> {
    let root = exe
        .parent()
        .ok_or("VS Code installation has no directory")?;
    // Read, but never execute, the official launcher's selected version path.
    let launcher = root.join("bin/code.cmd");
    if fs::metadata(&launcher).is_ok_and(|m| m.is_file() && m.len() <= 65536)
        && let Ok(text) = fs::read_to_string(launcher)
    {
        for token in text.split('"') {
            if let Some(relative) = token.strip_prefix("%~dp0..\\") {
                let relative = Path::new(relative);
                if relative.ends_with("resources/app/out/cli.js")
                    && relative
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)))
                {
                    let path = root.join(relative);
                    if path.is_file() {
                        return Ok(path);
                    }
                }
            }
        }
    }
    let direct = root.join("resources/app/out/cli.js");
    if direct.is_file() {
        return Ok(direct);
    }
    // VS Code's installed launcher can point into a versioned immediate directory.
    let mut selected = None;
    for entry in fs::read_dir(root).map_err(|e| e.to_string())?.take(128) {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            let path = entry.path().join("resources/app/out/cli.js");
            if path.is_file() {
                if selected.is_some() {
                    return Err("VS Code has several CLI versions and no usable installed launcher. Repair the installation.".into());
                }
                selected = Some(path);
            }
        }
    }
    selected.ok_or_else(|| {
        "Cannot locate VS Code's CLI wait launcher. Choose its installed Code.exe.".into()
    })
}
fn config_path() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?)
            .join("git-rustle/tools.json"),
    )
}
/// Stable for the lifetime of the saved entry, independent of its display name.
pub fn new_id() -> String {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "custom-{}-{time}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}
fn executable_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let text = if let Some(unc) = text.strip_prefix("\\\\?\\unc\\") {
        format!("\\\\{unc}")
    } else {
        text.strip_prefix("\\\\?\\").unwrap_or(&text).to_string()
    };
    let mut parts = Vec::new();
    for part in text.split('\\') {
        match part {
            "." => {}
            ".." if parts
                .last()
                .is_some_and(|p: &&str| !p.is_empty() && !p.ends_with(':')) =>
            {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("\\")
}
fn same_application(a: &Entry, b: &Entry) -> bool {
    same_executable(&a.executable, &b.executable)
}
/// Compare Windows path spelling without filesystem access or changing saved paths.
pub fn same_executable(a: &Path, b: &Path) -> bool {
    executable_key(a) == executable_key(b)
}
fn validate(config: &Config) -> Result<()> {
    if config.tools.len() > 128 {
        return Err("External tool settings exceed 128 entries.".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for entry in &config.tools {
        if entry.id.is_empty()
            || entry.id.len() > 128
            || !entry
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || !ids.insert(&entry.id)
        {
            return Err("External tool IDs must be unique, nonempty and bounded.".into());
        }
        if entry.name.trim().is_empty()
            || entry.name.len() > 256
            || entry.name.chars().any(char::is_control)
        {
            return Err("Give each external tool a name of at most 256 bytes.".into());
        }
        if entry.diff_arguments.is_none() && entry.merge_arguments.is_none() {
            return Err(format!("{} needs diff or merge arguments.", entry.name));
        }
        if let Some(tool) = entry.diff_tool() {
            tool.validate_configuration(false)?;
        }
        if let Some(tool) = entry.merge_tool() {
            tool.validate_configuration(true)?;
        }
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Legacy {
    diff: Option<Tool>,
    merge: Option<Tool>,
}
fn decode(bytes: &[u8]) -> Result<Config> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let object = value
        .as_object()
        .ok_or("External tool settings must be a JSON object")?;
    let config = if object.contains_key("tools") || object.contains_key("scanned") {
        serde_json::from_value(value).map_err(|e| e.to_string())?
    } else {
        let legacy: Legacy = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let mut tools: Vec<Entry> = Vec::new();
        for (merge, tool) in [(false, legacy.diff), (true, legacy.merge)] {
            let Some(tool) = tool else {
                continue;
            };
            let key = executable_key(&tool.executable);
            let position = tools
                .iter()
                .position(|e| e.preset == tool.preset && executable_key(&e.executable) == key);
            let i = position.unwrap_or_else(|| {
                // A deterministic legacy id keeps repeated pure reads stable.
                let mut hash = 0xcbf29ce484222325u64;
                let preset_key = format!("{:?}", tool.preset);
                for byte in key.bytes().chain(preset_key.bytes()) {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                tools.push(Entry {
                    id: format!("legacy-{hash:016x}"),
                    name: match tool.preset {
                        Preset::VsCode => "VS Code".into(),
                        Preset::Araxis => "Araxis Merge".into(),
                        Preset::Custom => tool
                            .executable
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Custom tool".into()),
                    },
                    executable: tool.executable.clone(),
                    preset: tool.preset,
                    diff_arguments: None,
                    merge_arguments: None,
                });
                tools.len() - 1
            });
            if merge {
                tools[i].merge_arguments = Some(tool.arguments);
            } else {
                tools[i].diff_arguments = Some(tool.arguments);
            }
        }
        Config {
            scanned: false,
            tools,
        }
    };
    validate(&config)?;
    Ok(config)
}
fn load_at(path: &Path) -> Result<Config> {
    match fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e.to_string()),
        Ok(m) if !m.is_file() || m.len() > 65536 => {
            Err("External tool settings must be a regular file no larger than 64 KiB.".into())
        }
        Ok(_) => {
            let mut bytes = Vec::new();
            fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(65537)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 65536 {
                return Err("External tool settings exceed 64 KiB.".into());
            }
            decode(&bytes)
        }
    }
}
/// Reading saved entries never scans installed applications or changes the file.
pub fn load() -> Result<Config> {
    load_at(&config_path()?)
}
struct SettingsGuard {
    _file: fs::File,
}
impl SettingsGuard {
    fn acquire(path: &Path) -> Result<Self> {
        fs::create_dir_all(
            path.parent()
                .ok_or("External tool settings have no directory")?,
        )
        .map_err(|e| e.to_string())?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options
            .open(path.with_extension("json.guard"))
            .map_err(|e| {
                format!("Tool settings are busy. Retry after the other operation finishes: {e}")
            })?;
        Ok(Self { _file: file })
    }
}
fn persist(path: &Path, config: &Config) -> Result<()> {
    validate(config)?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("External tool settings exceed 64 KiB.".into());
    }
    let temp = path.with_extension(format!("json.{}.tmp", new_id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn save_at(path: &Path, incoming: &Config) -> Result<Config> {
    validate(incoming)?;
    let _guard = SettingsGuard::acquire(path)?;
    let disk = load_at(path)?;
    let mut effective = incoming.clone();
    if disk.scanned {
        effective.scanned = true;
        if !incoming.scanned {
            // This editor opened before discovery completed. Preserve only entries
            // it could not have reviewed, without reviving deliberately removed ones.
            for entry in disk
                .tools
                .into_iter()
                .filter(|e| e.id.starts_with("detected-"))
            {
                if !effective
                    .tools
                    .iter()
                    .any(|e| e.id == entry.id || same_application(e, &entry))
                {
                    effective.tools.push(entry);
                }
            }
        }
    }
    persist(path, &effective)?;
    Ok(effective)
}
/// The returned value includes any startup discovery racing an older editor draft.
pub fn save(config: &Config) -> Result<Config> {
    save_at(&config_path()?, config)
}
fn discovery_roots() -> Vec<PathBuf> {
    // Only explicitly marked disposable native fixtures may replace discovery roots.
    if let Some(appdata) = std::env::var_os("LOCALAPPDATA") {
        let marker = PathBuf::from(appdata).join(".git-rustle-tools-list-fixture");
        if fs::read_to_string(marker).is_ok_and(|s| s == "disposable external tools list fixture")
            && let Some(paths) = std::env::var_os("GIT_RUSTLE_TOOL_DISCOVERY_ROOTS")
        {
            return std::env::split_paths(&paths).take(256).collect();
        }
    }
    let mut roots = Vec::new();
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(p).join("Programs/Microsoft VS Code"));
    }
    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(p) = std::env::var_os(key) {
            let p = PathBuf::from(p);
            roots.push(p.join("Microsoft VS Code"));
            roots.push(p.join("Araxis/Araxis Merge"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        roots.extend(std::env::split_paths(&path).take(256));
    }
    roots
}
pub fn detect() -> Vec<Entry> {
    detect_in(&discovery_roots())
}
fn detect_in(roots: &[PathBuf]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for root in roots {
        for (name, preset) in [
            ("Code.exe", Preset::VsCode),
            ("Compare.exe", Preset::Araxis),
        ] {
            if entries.iter().any(|e: &Entry| e.preset == preset) {
                continue;
            }
            let executable = root.join(name);
            if !executable.is_file() || preset == Preset::VsCode && vscode_cli(&executable).is_err()
            {
                continue;
            }
            let executable = executable.canonicalize().unwrap_or(executable);
            let entry = presets(executable, preset);
            if !entries
                .iter()
                .any(|e| executable_key(&e.executable) == executable_key(&entry.executable))
            {
                entries.push(entry);
            }
        }
        if entries.len() == 2 {
            break;
        }
    }
    entries
}
fn initialize_at(path: &Path, discover: impl FnOnce() -> Vec<Entry>) -> Result<Config> {
    let _guard = SettingsGuard::acquire(path)?;
    let mut config = load_at(path)?;
    if config.scanned {
        return Ok(config);
    }
    for entry in discover() {
        if !config
            .tools
            .iter()
            .any(|e| e.id == entry.id || same_application(e, &entry))
        {
            config.tools.push(entry);
        }
    }
    config.scanned = true;
    persist(path, &config)?;
    Ok(config)
}
/// Call once from the idle startup worker. A successful empty scan is persisted too.
pub fn load_or_scan_once() -> Result<Config> {
    load_or_scan_at(&config_path()?, detect)
}
fn load_or_scan_at(path: &Path, discover: impl FnOnce() -> Vec<Entry>) -> Result<Config> {
    // A saved flag makes subsequent launches a pure bounded config read.
    let current = load_at(path)?;
    if current.scanned {
        return Ok(current);
    }
    initialize_at(path, discover)
}
fn presets(executable: PathBuf, preset: Preset) -> Entry {
    let (diff, merge): (&[&str], &[&str]) = match preset {
        Preset::VsCode => (
            &["--wait", "--diff", "{left}", "{right}"],
            &[
                "--wait", "--merge", "{right}", "{left}", "{base}", "{output}",
            ],
        ),
        _ => (
            &["/wait", "/2", "/readOnly", "{left}", "{right}"],
            &[
                "/wait", "/3", "/a2", "{left}", "{base}", "{right}", "{output}",
            ],
        ),
    };
    Entry {
        id: match preset {
            Preset::VsCode => "detected-vscode",
            _ => "detected-araxis",
        }
        .into(),
        name: match preset {
            Preset::VsCode => "VS Code",
            _ => "Araxis Merge",
        }
        .into(),
        executable,
        preset,
        diff_arguments: Some(diff.iter().map(|s| (*s).into()).collect()),
        merge_arguments: Some(merge.iter().map(|s| (*s).into()).collect()),
    }
}

struct Session {
    path: PathBuf,
    files: Vec<PathBuf>,
}
impl Session {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join("git-rustle-tools");
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = root.join(format!(
            "{}-{now}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self {
            path,
            files: Vec::new(),
        })
    }
    fn input(&mut self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        if bytes.len() as u64 > LIMIT {
            return Err("External tool inputs are limited to 8 MiB each.".into());
        }
        let path = self.path.join(name);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        self.files.push(path.clone());
        Ok(path)
    }
    fn cleanup(self) {
        for p in self.files {
            let _ = fs::remove_file(p);
        }
        let _ = fs::remove_dir(self.path);
    }
    fn launch(self, tool: &Tool, paths: BTreeMap<&str, PathBuf>, merge: bool) -> Result<()> {
        let result = tool
            .command(&paths, merge)
            .and_then(|mut c| {
                c.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map_err(|e| e.to_string())
            })
            .and_then(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err(format!("Tool exited with {s}"))
                }
            });
        match result {
            Ok(()) => {
                self.cleanup();
                Ok(())
            }
            Err(e) => Err(format!(
                "{e}. Input copies retained at {}",
                self.path.display()
            )),
        }
    }
}
pub fn diff(root: &Path, change: &crate::diff::FileChange, tool: &Tool) -> Result<()> {
    let repo = crate::git::open(root).map_err(|e| e.to_string())?;
    let (left, right) = crate::diff::external_contents(&repo, change).map_err(|e| e.to_string())?;
    let mut session = Session::new()?;
    let paths = BTreeMap::from([
        ("{left}", session.input("left", &left)?),
        ("{right}", session.input("right", &right)?),
    ]);
    session.launch(tool, paths, false)
}
pub fn merge(root: &Path, path: &str, tool: &Tool) -> Result<()> {
    use gix::bstr::ByteSlice;
    use gix::prelude::FindExt;
    let _guard = crate::git::operations::repository_edit_guard(root)?;
    let relative = Path::new(path);
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("Invalid merge output path.".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let output = root.join(relative);
    let mut cursor = root.clone();
    for part in relative.components() {
        cursor.push(part);
        if fs::symlink_metadata(&cursor).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("Merge output cannot follow symbolic links.".into());
        }
    }
    let parent = output
        .parent()
        .ok_or("Invalid merge output path")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !parent.starts_with(&root) {
        return Err("Merge output is outside the working tree.".into());
    }
    let repo = crate::git::open(&root).map_err(|e| e.to_string())?;
    let index = repo.index().map_err(|e| e.to_string())?;
    let mut stages = BTreeMap::new();
    for entry in index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() != 0 && e.path(&index).as_bstr() == path.as_bytes().as_bstr())
    {
        if entry.mode == gix::index::entry::Mode::COMMIT {
            return Err("Resolve submodule conflicts inside the submodule.".into());
        }
        if entry.mode == gix::index::entry::Mode::SYMLINK {
            return Err("Use a whole-side choice for symbolic-link conflicts.".into());
        }
        let mut buf = Vec::new();
        let bytes = repo
            .objects
            .find_blob(&entry.id, &mut buf)
            .map_err(|e| e.to_string())?
            .data
            .to_vec();
        stages.insert(entry.stage_raw(), bytes);
    }
    if stages.is_empty() {
        return Err("The selected file is no longer conflicted. Refresh conflicts.".into());
    }
    let mut session = Session::new()?;
    let mut paths = BTreeMap::new();
    for (key, name, stage) in [
        ("{base}", "base", 1),
        ("{left}", "ours", 2),
        ("{right}", "theirs", 3),
    ] {
        paths.insert(
            key,
            session.input(
                name,
                stages.get(&stage).map(Vec::as_slice).unwrap_or_default(),
            )?,
        );
    }
    if output.exists() {
        let meta = fs::metadata(&output).map_err(|e| e.to_string())?;
        if meta.len() > LIMIT {
            return Err("Merge output exceeds 8 MiB.".into());
        }
        session.input(
            "before-merge",
            &fs::read(&output).map_err(|e| e.to_string())?,
        )?;
    }
    paths.insert("{output}", output);
    session.launch(tool, paths, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn installation(root: &Path) {
        fs::create_dir_all(root.join("resources/app/out")).unwrap();
        fs::write(root.join("Code.exe"), b"fixture executable, never launched").unwrap();
        fs::write(root.join("resources/app/out/cli.js"), b"fixture CLI").unwrap();
        fs::write(
            root.join("Compare.exe"),
            b"fixture executable, never launched",
        )
        .unwrap();
    }
    fn custom(name: &str) -> Entry {
        let tool = helper(false);
        Entry {
            id: new_id(),
            name: name.into(),
            executable: tool.executable,
            preset: Preset::Custom,
            diff_arguments: Some(tool.arguments),
            merge_arguments: None,
        }
    }
    #[test]
    fn discovery_collects_both_presets_once_and_deduplicates_installations() {
        let dir = tempfile::tempdir().unwrap();
        installation(dir.path());
        let found = detect_in(&[dir.path().into(), dir.path().join("."), dir.path().into()]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].preset, Preset::VsCode);
        assert_eq!(found[1].preset, Preset::Araxis);
        assert!(
            found
                .iter()
                .all(|e| e.diff_tool().is_some() && e.merge_tool().is_some())
        );
        assert_eq!(found, detect_in(&[dir.path().into()]));
        let path = dir.path().join("settings/tools.json");
        let config = initialize_at(&path, || found.clone()).unwrap();
        assert!(config.scanned);
        assert_eq!(config.tools, found);
        assert_eq!(
            initialize_at(&path, || panic!("discovery repeated")).unwrap(),
            config
        );
    }
    #[test]
    fn legacy_migration_is_lossless_and_pure_reads_do_not_scan_or_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools.json");
        let diff = helper(false);
        let merge = helper(true);
        let bytes = serde_json::to_vec(&serde_json::json!({"diff":diff,"merge":merge})).unwrap();
        fs::write(&path, &bytes).unwrap();
        let config = load_at(&path).unwrap();
        assert!(!config.scanned);
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].diff_tool(), Some(diff));
        assert_eq!(config.tools[0].merge_tool(), Some(merge));
        assert_eq!(config, load_at(&path).unwrap());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let migrated = initialize_at(&path, Vec::new).unwrap();
        assert!(migrated.scanned);
        assert_eq!(migrated.tools, config.tools);
        let other = Tool {
            executable: dir.path().join("uninstalled.exe"),
            arguments: vec!["{left}".into(), "{right}".into()],
            preset: Preset::Custom,
        };
        let different = decode(
            &serde_json::to_vec(&serde_json::json!({"diff":other,"merge":helper(true)})).unwrap(),
        )
        .unwrap();
        assert_eq!(different.tools.len(), 2);
        assert!(
            different.tools[0]
                .diff_tool()
                .unwrap()
                .validate(false)
                .is_err()
        );
    }
    #[test]
    fn empty_scan_and_user_deletions_never_resurrect_discovered_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools.json");
        let empty = initialize_at(&path, Vec::new).unwrap();
        assert!(empty.scanned && empty.tools.is_empty());
        installation(dir.path());
        assert_eq!(
            initialize_at(&path, || panic!("empty scan repeated")).unwrap(),
            empty
        );
        let second = dir.path().join("second.json");
        let found = initialize_at(&second, || detect_in(&[dir.path().into()])).unwrap();
        assert_eq!(found.tools.len(), 2);
        let deleted = save_at(
            &second,
            &Config {
                scanned: true,
                tools: Vec::new(),
            },
        )
        .unwrap();
        assert!(deleted.scanned && deleted.tools.is_empty());
        assert_eq!(
            initialize_at(&second, || panic!("deleted tools rediscovered")).unwrap(),
            deleted
        );
    }
    #[test]
    fn scan_preserves_custom_arguments_and_saves_racing_discovery_preserve_edits() {
        let dir = tempfile::tempdir().unwrap();
        installation(dir.path());
        let path = dir.path().join("tools.json");
        let mut old = Config {
            scanned: false,
            tools: vec![custom("Before scan")],
        };
        save_at(&path, &old).unwrap();
        let scanned = initialize_at(&path, || detect_in(&[dir.path().into()])).unwrap();
        assert_eq!(scanned.tools.len(), 3);
        old.tools[0].name = "Edited while startup completed".into();
        let merged = save_at(&path, &old).unwrap();
        assert!(merged.scanned);
        assert_eq!(merged.tools.len(), 3);
        assert_eq!(merged.tools[0].name, old.tools[0].name);
        assert_eq!(merged.tools[0].diff_arguments, old.tools[0].diff_arguments);
        let second = dir.path().join("custom-code.json");
        let mut customized = custom("Custom VS Code arguments");
        customized.executable = dir.path().join("Code.exe");
        let exact = customized.clone();
        save_at(
            &second,
            &Config {
                scanned: false,
                tools: vec![customized],
            },
        )
        .unwrap();
        let result = initialize_at(&second, || detect_in(&[dir.path().into()])).unwrap();
        assert_eq!(result.tools.len(), 2);
        assert_eq!(result.tools[0], exact);
        assert_eq!(result.tools[1].preset, Preset::Araxis);
    }
    #[test]
    fn corrupt_configs_and_concurrent_writes_are_not_silently_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools.json");
        let config = Config {
            scanned: false,
            tools: vec![custom("Keep me")],
        };
        save_at(&path, &config).unwrap();
        let bytes = fs::read(&path).unwrap();
        let guard = SettingsGuard::acquire(&path).unwrap();
        assert!(save_at(&path, &config).is_err());
        assert!(initialize_at(&path, || panic!("must hold the guard before scanning")).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        drop(guard);
        save_at(&path, &config).unwrap();
        fs::write(&path, b"{corrupt").unwrap();
        assert!(initialize_at(&path, || panic!("invalid config must not scan")).is_err());
        assert!(save_at(&path, &config).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{corrupt");
    }
    #[test]
    fn executable_identity_is_case_insensitive_without_changing_saved_arguments() {
        assert!(same_executable(
            Path::new("C:/Tools/../Tools/Code.exe"),
            Path::new(r"\\?\C:\TOOLS\Code.EXE")
        ));
        assert!(same_executable(
            Path::new(r"\\?\UNC\server\share\Code.exe"),
            Path::new(r"\\server\share\CODE.exe")
        ));
        assert!(!same_executable(
            Path::new(r"C:\TOOLS\Code.exe"),
            Path::new(r"C:\OTHER\Code.exe")
        ));
        assert!(!same_executable(
            Path::new(r"C:\TOOLS\Code.exe"),
            Path::new(r"C:\TOOLS\Compare.exe")
        ));
    }
    fn helper(merge: bool) -> Tool {
        let mut arguments = vec![
            "--exact",
            "external_tools::tests::tool_child",
            "--ignored",
            "--skip",
            "rustle-tool-child",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        for value in if merge {
            vec!["{left}", "{right}", "{base}", "{output}"]
        } else {
            vec!["{left}", "{right}"]
        } {
            arguments.extend(["--skip".into(), value.into()]);
        }
        Tool {
            executable: std::env::current_exe().unwrap(),
            arguments,
            preset: Preset::Custom,
        }
    }
    #[test]
    #[ignore = "Controlled child process launched only by external tool fixtures"]
    fn tool_child() {
        let args = std::env::args().collect::<Vec<_>>();
        let paths = args
            .windows(2)
            .filter(|p| p[0] == "--skip")
            .map(|p| p[1].clone())
            .collect::<Vec<_>>();
        if paths.first().map(String::as_str) != Some("rustle-tool-child") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(fs::read(&paths[1]).unwrap(), b"ours\0\r\n");
        assert_eq!(fs::read(&paths[2]).unwrap(), b"theirs\0\n");
        if paths.len() == 5 {
            assert_eq!(fs::read(&paths[3]).unwrap(), b"base\0\n");
            fs::write(&paths[4], b"merged\r\n").unwrap();
        }
    }
    #[test]
    fn arguments_are_literal_and_scripts_are_rejected() {
        let tool = helper(false);
        let left = PathBuf::from("C:/file with spaces/$(touch);& left");
        let cmd = tool
            .command(
                &BTreeMap::from([
                    ("{left}", left.clone()),
                    ("{right}", PathBuf::from("C:/right")),
                ]),
                false,
            )
            .unwrap();
        assert!(cmd.get_args().any(|a| a == left.as_os_str()));
        let mut script = tool;
        script.executable.set_extension("cmd");
        assert!(script.validate(false).is_err());
    }
    #[test]
    fn vscode_detection_uses_cli_wait_process_and_correct_side_order() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Code.exe");
        fs::write(&exe, b"fixture").unwrap();
        let cli = dir.path().join("version/resources/app/out/cli.js");
        fs::create_dir_all(cli.parent().unwrap()).unwrap();
        fs::write(&cli, b"fixture").unwrap();
        let config = detect_in(&[dir.path().to_path_buf()]);
        let tool = config[0].merge_tool().unwrap();
        let paths = BTreeMap::from([
            ("{left}", PathBuf::from("ours")),
            ("{right}", PathBuf::from("theirs")),
            ("{base}", PathBuf::from("base")),
            ("{output}", PathBuf::from("output")),
        ]);
        let cmd = tool.command(&paths, true).unwrap();
        let args = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            PathBuf::from(&args[0]).canonicalize().unwrap(),
            cli.canonicalize().unwrap()
        );
        assert_eq!(
            &args[1..],
            &["--wait", "--merge", "theirs", "ours", "base", "output"]
        );
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "ELECTRON_RUN_AS_NODE" && v == Some(std::ffi::OsStr::new("1")))
        );
        // Manually added VS Code remains a custom entry on disk, but uses the
        // same waiting CLI at runtime instead of its forwarding GUI process.
        for merge in [false, true] {
            let mut custom = if merge {
                config[0].merge_tool().unwrap()
            } else {
                config[0].diff_tool().unwrap()
            };
            custom.preset = Preset::Custom;
            let original_arguments = custom.arguments.clone();
            let cmd = custom.command(&paths, merge).unwrap();
            let args = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                PathBuf::from(&args[0]).canonicalize().unwrap(),
                cli.canonicalize().unwrap()
            );
            if merge {
                assert_eq!(
                    &args[1..],
                    &["--wait", "--merge", "theirs", "ours", "base", "output"]
                );
            } else {
                assert_eq!(&args[1..], &["--wait", "--diff", "ours", "theirs"]);
            }
            assert!(
                cmd.get_envs()
                    .any(|(k, v)| k == "ELECTRON_RUN_AS_NODE"
                        && v == Some(std::ffi::OsStr::new("1")))
            );
            assert!(
                cmd.get_envs()
                    .any(|(k, v)| k == "VSCODE_DEV" && v.is_none())
            );
            assert_eq!(custom.preset, Preset::Custom);
            assert_eq!(custom.arguments, original_arguments);
        }
        let unavailable = tempfile::tempdir().unwrap();
        let exe_without_cli = unavailable.path().join("Code.exe");
        fs::write(&exe_without_cli, b"fixture").unwrap();
        for merge in [false, true] {
            let mut manual = if merge {
                config[0].merge_tool().unwrap()
            } else {
                config[0].diff_tool().unwrap()
            };
            manual.preset = Preset::Custom;
            manual.executable = exe_without_cli.clone();
            assert!(manual.command(&paths, merge).is_err());
        }
        assert!(!helper(false).uses_vscode_cli(false));
        let stale = dir.path().join("old/resources/app/out/cli.js");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(stale, b"stale").unwrap();
        assert!(vscode_cli(&exe).is_err());
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        fs::write(
            dir.path().join("bin/code.cmd"),
            "\"%~dp0..\\Code.exe\" \"%~dp0..\\version\\resources\\app\\out\\cli.js\" %*",
        )
        .unwrap();
        assert_eq!(
            vscode_cli(&exe).unwrap().canonicalize().unwrap(),
            cli.canonicalize().unwrap()
        );
    }
    #[test]
    fn child_wait_retains_exact_inputs_then_cleans_up() {
        let mut session = Session::new().unwrap();
        let directory = session.path.clone();
        let paths = BTreeMap::from([
            ("{left}", session.input("left", b"ours\0\r\n").unwrap()),
            ("{right}", session.input("right", b"theirs\0\n").unwrap()),
        ]);
        session.launch(&helper(false), paths, false).unwrap();
        assert!(!directory.exists());
    }
    fn merge_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| crate::git::command::checked(dir.path(), args).unwrap();
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Tools Test"]);
        git(&["config", "user.email", "tools@example.invalid"]);
        git(&["config", "commit.gpgSign", "false"]);
        git(&["config", "core.autocrlf", "false"]);
        fs::write(dir.path().join("file.bin"), b"base\0\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "base"]);
        git(&["switch", "-c", "other"]);
        fs::write(dir.path().join("file.bin"), b"theirs\0\n").unwrap();
        git(&["commit", "-am", "theirs"]);
        git(&["switch", "main"]);
        fs::write(dir.path().join("file.bin"), b"ours\0\r\n").unwrap();
        git(&["commit", "-am", "ours"]);
        assert!(crate::git::command::checked(dir.path(), &["merge", "other"]).is_err());
        dir
    }
    #[test]
    fn merge_reads_exact_stages_and_leaves_result_unstaged() {
        let dir = merge_fixture();
        let before = crate::git::command::checked(dir.path(), &["ls-files", "--unmerged"]).unwrap();
        merge(dir.path(), "file.bin", &helper(true)).unwrap();
        assert_eq!(
            fs::read(dir.path().join("file.bin")).unwrap(),
            b"merged\r\n"
        );
        assert_eq!(
            before,
            crate::git::command::checked(dir.path(), &["ls-files", "--unmerged"]).unwrap()
        );
        assert!(merge(dir.path(), "../outside", &helper(true)).is_err());
    }
    #[test]
    fn external_diff_uses_exact_binary_worktree_bytes_without_staging() {
        let dir = merge_fixture();
        let id = crate::git::command::checked(dir.path(), &["rev-parse", "HEAD:file.bin"]).unwrap();
        let id = gix::ObjectId::from_hex(id.as_slice().trim_ascii()).unwrap();
        let before = crate::git::command::checked(dir.path(), &["ls-files", "--stage"]).unwrap();
        fs::write(dir.path().join("file.bin"), b"theirs\0\n").unwrap();
        let change = crate::diff::FileChange {
            worktree: true,
            remote: None,
            path: "file.bin".into(),
            old_path: None,
            kind: crate::diff::ChangeKind::Modified,
            old: Some(crate::diff::Side {
                id,
                kind: gix::objs::tree::EntryKind::Blob,
            }),
            new: None,
        };
        diff(dir.path(), &change, &helper(false)).unwrap();
        assert_eq!(
            fs::read(dir.path().join("file.bin")).unwrap(),
            b"theirs\0\n"
        );
        assert_eq!(
            before,
            crate::git::command::checked(dir.path(), &["ls-files", "--stage"]).unwrap()
        );
    }
    #[test]
    #[ignore = "One warmup plus fifteen persistent list discovery/load/save/migration samples"]
    fn benchmark_tool_list() {
        use std::{hint::black_box, time::Instant};
        for sample in 0..16 {
            let dir = tempfile::tempdir().unwrap();
            installation(dir.path());
            let path = dir.path().join("config/tools.json");
            let start = Instant::now();
            let mut config = load_or_scan_at(&path, || detect_in(&[dir.path().into()])).unwrap();
            let first_scan_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(config.tools.len(), 2);
            let start = Instant::now();
            let repeated =
                load_or_scan_at(&path, || panic!("repeat launch must not discover")).unwrap();
            let repeat_load_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(config, repeated);
            let start = Instant::now();
            for i in 0..1000 {
                assert!(black_box(config.entry(black_box(&config.tools[i % 2].id))).is_some());
            }
            let selection_1000_ms = start.elapsed().as_secs_f64() * 1000.;
            config.tools[0].name = "My configured editor".into();
            let start = Instant::now();
            let saved = save_at(&path, &config).unwrap();
            let save_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(saved.tools[0].name, "My configured editor");
            let legacy = dir.path().join("legacy.json");
            fs::write(
                &legacy,
                serde_json::to_vec(&serde_json::json!({"diff":helper(false),"merge":helper(true)}))
                    .unwrap(),
            )
            .unwrap();
            let start = Instant::now();
            let migrated = load_or_scan_at(&legacy, Vec::new).unwrap();
            let migrate_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(migrated.scanned);
            assert_eq!(migrated.tools.len(), 1);
            if sample > 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"first_scan_ms":first_scan_ms,"repeat_load_ms":repeat_load_ms,"save_ms":save_ms,"migrate_ms":migrate_ms,"selection_1000_ms":selection_1000_ms,"tools":config.tools.len()})
                );
            }
        }
    }
    #[test]
    #[ignore = "One warmup plus fifteen controlled discovery and real process samples"]
    fn benchmark_external_tools() {
        use std::time::Instant;
        let installation = tempfile::tempdir().unwrap();
        fs::write(installation.path().join("Code.exe"), b"fixture").unwrap();
        let cli = installation.path().join("resources/app/out/cli.js");
        fs::create_dir_all(cli.parent().unwrap()).unwrap();
        fs::write(cli, b"fixture").unwrap();
        for sample in 0..16 {
            let start = Instant::now();
            let config = detect_in(&[installation.path().to_path_buf()]);
            assert!(config[0].diff_tool().is_some());
            let detect_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            let mut session = Session::new().unwrap();
            let paths = BTreeMap::from([
                ("{left}", session.input("left", b"ours\0\r\n").unwrap()),
                ("{right}", session.input("right", b"theirs\0\n").unwrap()),
            ]);
            let prepare_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            session.launch(&helper(false), paths, false).unwrap();
            let launch_wait_ms = start.elapsed().as_secs_f64() * 1000.;
            let dir = merge_fixture();
            let start = Instant::now();
            merge(dir.path(), "file.bin", &helper(true)).unwrap();
            let merge_ms = start.elapsed().as_secs_f64() * 1000.;
            if sample > 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"detect_ms":detect_ms,"prepare_ms":prepare_ms,"launch_wait_ms":launch_wait_ms,"merge_ms":merge_ms})
                );
            }
        }
    }
}
