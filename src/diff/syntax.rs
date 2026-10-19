//! Deferred syntax coloring. No parsing, queries, or source scans on the UI thread.
mod xml;

use super::sidebyside::decode;
use gpui_component::highlighter::LanguageRegistry;
use std::{
    collections::HashMap,
    ops::Range,
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

// Bounds apply independently to both sides. Large/generated files remain usable as plain text.
const MAX_SOURCE: usize = 512 * 1024;
const MAX_NORMALIZED: usize = 1024 * 1024;
const MAX_LINE: usize = 16 * 1024;
const MAX_SPANS: usize = 100_000;
const BUDGET: Duration = Duration::from_millis(150);
const NAMES: &[&str] = &[
    "comment",
    "string",
    "keyword",
    "function",
    "type",
    "number",
    "constant",
    "property",
    "operator",
    "punctuation",
    "variable",
    "tag",
    "attribute",
    "constructor",
    "boolean",
    "embedded",
    "title",
    "link_uri",
    "text.literal",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// UTF-8 byte range in the decoded (tab-expanded) source line.
    pub range: Range<usize>,
    pub style: u8,
}

#[derive(Debug, Default)]
pub struct Side {
    spans: Vec<Span>,
    lines: Vec<Range<usize>>,
}
impl Side {
    pub fn line(&self, number: u32) -> &[Span] {
        self.lines
            .get(number.saturating_sub(1) as usize)
            .map_or(&[], |r| &self.spans[r.clone()])
    }
}
#[derive(Debug, Default)]
pub struct Syntax {
    pub sides: [Side; 2],
}
impl Syntax {
    pub fn span_count(&self) -> usize {
        self.sides.iter().map(|s| s.spans.len()).sum()
    }
}

struct Job {
    id: (u32, u32, usize),
    source: [Arc<[u8]>; 2],
    paths: [String; 2],
    cancel: Arc<AtomicUsize>,
}
#[derive(Default)]
struct Mailbox {
    job: Option<Job>,
    cancel: Option<Arc<AtomicUsize>>,
    closed: bool,
}
pub struct Worker(Arc<(Mutex<Mailbox>, Condvar)>);
impl Worker {
    pub fn new(tx: async_channel::Sender<crate::engine::Msg>) -> Self {
        let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let shared = mailbox.clone();
        thread::Builder::new()
            .name("diff-syntax".into())
            .spawn(move || {
                let mut painter = Painter::default();
                loop {
                    let job = {
                        let (lock, wake) = &*shared;
                        let mut state = lock.lock().unwrap();
                        while state.job.is_none() && !state.closed {
                            state = wake.wait(state).unwrap();
                        }
                        if state.closed {
                            return;
                        }
                        state.job.take().unwrap()
                    };
                    // The watchdog cancels Tree-sitter during parsing and event traversal.
                    // Query compilation is cached and cannot itself be interrupted.
                    let result = thread::scope(|scope| {
                        let (done, wait) = mpsc::channel();
                        let cancel = job.cancel.clone();
                        scope.spawn(move || {
                            if wait.recv_timeout(BUDGET).is_err() {
                                cancel.store(1, Ordering::Relaxed);
                            }
                        });
                        let sides = std::array::from_fn(|i| {
                            painter.side(&job.paths[i], &job.source[i], &job.cancel)
                        });
                        let _ = done.send(());
                        Syntax { sides }
                    });
                    if tx
                        .send_blocking(crate::engine::Msg::DiffSyntax(
                            job.id.0,
                            job.id.1,
                            job.id.2,
                            Arc::new(result),
                        ))
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .expect("spawn syntax worker");
        Self(mailbox)
    }

    pub fn submit(&self, id: (u32, u32, usize), source: [Arc<[u8]>; 2], paths: [String; 2]) {
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().unwrap();
        if let Some(cancel) = state.cancel.take() {
            cancel.store(1, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicUsize::new(0));
        state.cancel = Some(cancel.clone());
        // One pending request at most; superseded source buffers are released here.
        state.job = Some(Job {
            id,
            source,
            paths,
            cancel,
        });
        wake.notify_one();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().unwrap();
        state.closed = true;
        state.job = None;
        if let Some(cancel) = state.cancel.take() {
            cancel.store(1, Ordering::Relaxed);
        }
        wake.notify_one();
    }
}

#[derive(Default)]
struct Painter {
    configs: HashMap<String, Option<HighlightConfiguration>>,
    highlighter: Highlighter,
}

fn language(path: &str) -> Option<&'static str> {
    let path = Path::new(path);
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "dockerfile" => return None,
        "makefile" | "gnumakefile" => return Some("make"),
        "cmakelists.txt" => return Some("cmake"),
        "gemfile" | "rakefile" => return Some("ruby"),
        ".bashrc" | ".bash_profile" => return Some("bash"),
        _ => {}
    }
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "rs" => "rust",
            "c" | "h" => "c",
            "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => "cpp",
            "cs" | "csx" => "csharp",
            "xaml" | "axaml" | "xml" => "xml",
            "js" | "jsx" | "mjs" | "cjs" => "javascript",
            "ts" | "mts" | "cts" => "typescript",
            "tsx" => "tsx",
            "py" | "pyi" => "python",
            "json" | "jsonc" => "json",
            "toml" => "toml",
            "yaml" | "yml" => "yaml",
            "sh" | "bash" => "bash",
            "bat" | "cmd" => "batch",
            "ps1" | "psm1" | "psd1" => "powershell",
            "go" => "go",
            "rb" => "ruby",
            "java" => "java",
            "html" | "htm" => "html",
            "css" => "css",
            "sql" => "sql",
            "md" | "markdown" => "markdown",
            "swift" => "swift",
            "scala" | "sc" => "scala",
            "zig" => "zig",
            "ex" | "exs" => "elixir",
            "graphql" | "gql" => "graphql",
            "proto" => "proto",
            "cmake" => "cmake",
            "diff" | "patch" => "diff",
            _ => return None,
        },
    )
}
// Some grammar crates publish extension queries rather than a complete language.
fn configuration(lang: &str) -> Option<HighlightConfiguration> {
    // Supply queries missing from gpui-component and standalone grammars. Reuse
    // the bounded worker and map additional capture names onto our palette.
    let standalone = match lang {
        // gpui-component bundles the parser but exposes an empty C# query.
        "csharp" => Some((
            tree_sitter_c_sharp::LANGUAGE.into(),
            tree_sitter_c_sharp::HIGHLIGHTS_QUERY.replace("@module", "@type"),
        )),
        "batch" => Some((
            tree_sitter_batch::LANGUAGE.into(),
            tree_sitter_batch::HIGHLIGHTS_QUERY.replace("@label", "@title"),
        )),
        "powershell" => Some((
            tree_sitter_powershell::LANGUAGE.into(),
            tree_sitter_powershell::HIGHLIGHTS_QUERY.replace("@delimiter", "@punctuation"),
        )),
        _ => None,
    };
    if let Some((language, query)) = standalone {
        let mut result = HighlightConfiguration::new(language, lang, &query, "", "").ok()?;
        result.configure(NAMES);
        return Some(result);
    }
    let registry = LanguageRegistry::singleton();
    let config = registry.language(lang)?;
    let query = match lang {
        "cpp" => format!(
            "{}\n{}",
            registry.language("c")?.highlights,
            config.highlights
        ),
        "tsx" => format!(
            "{}\n{}",
            registry.language("typescript")?.highlights,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
        ),
        "javascript" => format!(
            "{}\n{}",
            config.highlights,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
        ),
        "cmake" => tree_sitter_cmake::HIGHLIGHTS_QUERY.replace(
            "#lua-match? @constant \"^[%u@][%u%d_]+$\"",
            "#match? @constant \"^[A-Z@][A-Z0-9_]+$\"",
        ),
        _ => config.highlights.to_string(),
    };
    // Primary language only: avoid recursively parsing injected languages.
    let mut result =
        HighlightConfiguration::new(config.language, lang, &query, "", &config.locals).ok()?;
    result.configure(NAMES);
    Some(result)
}

impl Painter {
    fn side(&mut self, path: &str, source: &[u8], cancel: &AtomicUsize) -> Side {
        self.try_side(path, source, cancel).unwrap_or_default()
    }

    fn try_side(&mut self, path: &str, source: &[u8], cancel: &AtomicUsize) -> Option<Side> {
        if source.is_empty()
            || source.len() > MAX_SOURCE
            || source.contains(&0)
            || cancel.load(Ordering::Relaxed) != 0
        {
            return None;
        }
        // Invalid UTF-8 still displays through the diff's lossy decoder, without colors.
        std::str::from_utf8(source).ok()?;
        let lang = language(path)?;
        // Parse complete source, including collapsed context, in exactly the same
        // representation that selection and wrapping use. This preserves multiline state.
        let mut text = String::with_capacity(source.len());
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        for raw in source.split_inclusive(|&b| b == b'\n') {
            if raw.len() > MAX_LINE {
                return None;
            }
            starts.push(text.len());
            text.push_str(&decode(raw));
            ends.push(text.len());
            text.push('\n');
            if text.len() > MAX_NORMALIZED {
                return None;
            }
        }
        if lang == "xml" {
            return xml::highlight(&text, &starts, &ends, cancel);
        }
        let config = self
            .configs
            .entry(lang.into())
            .or_insert_with(|| configuration(lang))
            .as_ref()?;
        if cancel.load(Ordering::Relaxed) != 0 {
            return None;
        }

        self.highlighter.parser().reset();
        let events = self
            .highlighter
            .highlight(config, text.as_bytes(), Some(cancel), |_| None)
            .ok()?;
        let mut result = Side {
            spans: Vec::new(),
            lines: vec![0..0; starts.len()],
        };
        let mut stack = Vec::new();
        let mut line = 0;
        for event in events {
            if cancel.load(Ordering::Relaxed) != 0 {
                return None;
            }
            match event.ok()? {
                HighlightEvent::HighlightStart(style) => stack.push(style.0 as u8),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    while line < starts.len() && starts[line] < end {
                        let a = start.max(starts[line]);
                        let b = end.min(ends[line]);
                        if a < b
                            && let Some(&style) = stack.last()
                        {
                            if !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                                return None;
                            }
                            let range = a - starts[line]..b - starts[line];
                            if let Some(last) = result.spans.last_mut()
                                && result.lines[line].start < result.lines[line].end
                                && last.style == style
                                && last.range.end == range.start
                            {
                                last.range.end = range.end;
                            } else {
                                result.spans.push(Span { range, style });
                                result.lines[line].end = result.spans.len();
                            }
                            if result.spans.len() > MAX_SPANS {
                                return None;
                            }
                        }
                        if end <= ends[line] {
                            break;
                        }
                        line += 1;
                        if line < starts.len() {
                            result.lines[line] = result.spans.len()..result.spans.len();
                        }
                    }
                }
            }
        }
        Some(result)
    }
}

/// Clip already cached byte spans for a wrapped fragment; no source scan.
pub fn clipped(spans: &[Span], range: Range<usize>) -> Vec<Span> {
    let first = spans.partition_point(|s| s.range.end <= range.start);
    spans[first..]
        .iter()
        .take_while(|s| s.range.start < range.end)
        .filter_map(|s| {
            let a = s.range.start.max(range.start);
            let b = s.range.end.min(range.end);
            (a < b).then_some(Span {
                range: a - range.start..b - range.start,
                style: s.style,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint(path: &str, text: &str) -> Side {
        Painter::default().side(path, text.as_bytes(), &AtomicUsize::new(0))
    }
    fn has(side: &Side, no: u32, line: &str, token: &str, category: &str) -> bool {
        let start = line.find(token).unwrap();
        side.line(no).iter().any(|s| {
            s.range.start <= start
                && s.range.end >= start + token.len()
                && NAMES[s.style as usize] == category
        })
    }

    #[test]
    fn rust_multiline_unicode_tabs_crlf_and_wrapped_spans() {
        let source = "/* hidden context\r\nstill a comment */\r\n\tlet café = \"世界\";\r\n";
        let side = paint("src/main.rs", source);
        assert!(has(&side, 2, "still a comment */", "still", "comment"));
        let text = decode(source.split_inclusive('\n').nth(2).unwrap().as_bytes());
        assert!(has(&side, 3, &text, "let", "keyword"));
        assert!(has(&side, 3, &text, "\"世界\"", "string"));
        let start = text.find('世').unwrap();
        let end = start + "世界".len();
        let clipped = clipped(side.line(3), start..end);
        assert_eq!(
            clipped,
            vec![Span {
                range: 0..6,
                style: 1
            }]
        );
        for (i, raw) in source.split_inclusive('\n').enumerate() {
            let line = decode(raw.as_bytes());
            let mut end = 0;
            for span in side.line(i as u32 + 1) {
                assert!(span.range.start >= end);
                assert!(line.get(span.range.clone()).is_some());
                end = span.range.end;
            }
        }
    }

    #[test]
    fn common_languages_have_real_tokens() {
        for (path, source, token, category) in [
            ("a.rs", "fn main() { let n = 42; }", "fn", "keyword"),
            ("a.py", "def greet(): return \"hi\"", "def", "keyword"),
            ("a.ts", "const value: number = 42;", "const", "keyword"),
            (
                "a.tsx",
                "const x = <Button name=\"hi\" />;",
                "\"hi\"",
                "string",
            ),
            ("a.js", "const answer = \"hello\";", "\"hello\"", "string"),
            ("a.c", "int main() { return 42; }", "return", "keyword"),
            ("a.cpp", "class Example {};", "class", "keyword"),
            ("a.cpp", "int main() { return 42; }", "return", "keyword"),
            ("CMakeLists.txt", "message(\"hello\")", "hello", "string"),
            ("a.json", "{\"answer\": 42}", "42", "number"),
            ("a.toml", "answer = 42", "42", "number"),
            ("a.yaml", "answer: \"hello\"", "\"hello\"", "string"),
        ] {
            let side = paint(path, source);
            assert!(
                has(&side, 1, source, token, category),
                "{path}: {:?}",
                side.spans
            );
        }
    }

    #[test]
    fn dotnet_extensions_are_case_insensitive() {
        for path in ["View.xaml.cs", "Program.CS", "script.CsX"] {
            assert_eq!(language(path), Some("csharp"));
        }
        for path in [
            "View.xaml",
            "View.XAML",
            "View.axaml",
            "App.AXAML",
            "data.XML",
        ] {
            assert_eq!(language(path), Some("xml"));
        }
    }

    #[test]
    fn csharp_has_real_tokens_and_multiline_context() {
        let source = "using System;\r\nnamespace Demo;\r\n[Obsolete]\r\npublic record Person(string Name);\r\npublic class View {\r\n/* hidden context\r\nstill a comment café */\r\npublic int GetCount() => 42;\r\nstring Message = $\"Hello {nameof(View)} 世界\";\r\nstring Raw = \"\"\"\r\nhello 世界\r\n\"\"\";\r\n}\r\n";
        let side = paint("View.xaml.CS", source);
        for (no, token, category) in [
            (1, "using", "keyword"),
            (3, "Obsolete", "attribute"),
            (4, "record", "keyword"),
            (4, "Person", "type"),
            (5, "class", "keyword"),
            (5, "View", "type"),
            (7, "still a comment café", "comment"),
            (8, "GetCount", "function"),
            (8, "int", "type"),
            (8, "42", "number"),
            (9, "Hello ", "string"),
            (11, "hello 世界", "string"),
        ] {
            let line = source.lines().nth(no as usize - 1).unwrap();
            assert!(
                has(&side, no, line, token, category),
                "{no}: {token} {category}: {:?}",
                side.line(no)
            );
        }
        let line = source.lines().nth(10).unwrap();
        let start = line.find('世').unwrap();
        assert_eq!(
            clipped(side.line(11), start..line.len()),
            vec![Span {
                range: 0..6,
                style: 1
            }]
        );
    }

    #[test]
    fn xaml_namespaces_properties_bindings_and_multiline_context() {
        let source = "<?xml version=\"1.0\"?>\r\n<Window x:Class=\"Demo.View\"\r\n xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\"\r\n xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\">\r\n<!-- hidden context\r\nstill a comment café -->\r\n<Grid><TextBlock x:Name=\"Greeting\" Grid.Row=\"1\"\r\n Text=\"{Binding Name, StringFormat='Hello 世界'}\" /></Grid>\r\n<TextBlock>hello &amp; goodbye</TextBlock>\r\n<![CDATA[raw 世界]]>\r\n</Window>\r\n";
        let side = paint("View.XAML", source);
        for (no, token, category) in [
            (1, "xml", "keyword"),
            (2, "Window", "tag"),
            (2, "x:Class", "property"),
            (2, "Demo.View", "string"),
            (4, "xmlns:x", "property"),
            (6, "still a comment café", "comment"),
            (7, "TextBlock", "tag"),
            (7, "Grid.Row", "property"),
            (8, "{Binding Name, StringFormat='Hello 世界'}", "string"),
            (9, "&amp;", "constant"),
            (10, "raw 世界", "string"),
            (11, "Window", "tag"),
        ] {
            let line = source.lines().nth(no as usize - 1).unwrap();
            assert!(
                has(&side, no, line, token, category),
                "{no}: {token} {category}: {:?}",
                side.line(no)
            );
        }
        let line = source.lines().nth(7).unwrap();
        let start = line.find('世').unwrap();
        assert_eq!(
            clipped(side.line(8), start..start + 6),
            vec![Span {
                range: 0..6,
                style: 1
            }]
        );
    }

    #[test]
    fn xml_incomplete_suffix_keeps_prefix_and_span_invariants() {
        for source in [
            "<Grid>\r\n\t<TextBlock Text=\"café 世界\" />\r\n<Button Text=\"unfinished",
            "<Grid>\n<TextBlock Text=\"hello\" />\n<!-- unfinished",
            "<Grid><TextBlock Text=\"hello\" />",
        ] {
            let side = paint("View.xaml", source);
            assert!(has(&side, 1, source.lines().next().unwrap(), "Grid", "tag"));
            for (i, raw) in source.split_inclusive('\n').enumerate() {
                let line = decode(raw.as_bytes());
                let mut end = 0;
                for span in side.line(i as u32 + 1) {
                    assert!(span.range.start >= end);
                    assert!(line.get(span.range.clone()).is_some());
                    end = span.range.end;
                }
            }
        }
    }

    #[test]
    fn xml_entities_dtd_and_resource_limits() {
        let source = "<?xml version=\"1.0\"?>\n<!DOCTYPE Grid [<!ENTITY name \"world\">]>\n<Grid>hello &name; &#x4e16; &amp;</Grid>";
        let side = paint("View.xaml", source);
        for token in ["&name;", "&#x4e16;", "&amp;"] {
            assert!(has(
                &side,
                3,
                source.lines().nth(2).unwrap(),
                token,
                "constant"
            ));
        }
        let mut painter = Painter::default();
        assert!(
            painter
                .side("x.xaml", b"<Grid/>", &AtomicUsize::new(1))
                .spans
                .is_empty()
        );
        assert!(paint("x.xaml", &"x".repeat(MAX_LINE + 1)).spans.is_empty());

        let dense = format!("<Grid>{}</Grid>", "<A/>\n".repeat(40_000));
        assert!(paint("x.xaml", &dense).spans.is_empty());
    }

    #[test]
    fn windows_shell_extensions_are_case_insensitive() {
        for path in ["build.bat", "build.cmd", "BUILD.BAT", "dir/Build.CmD"] {
            assert_eq!(language(path), Some("batch"));
        }
        for path in [
            "build.ps1",
            "module.psm1",
            "module.psd1",
            "BUILD.PS1",
            "dir/Test.PsM1",
        ] {
            assert_eq!(language(path), Some("powershell"));
        }
    }

    #[test]
    fn batch_commands_comments_labels_and_expansions() {
        let source = "@echo off\r\nREM build café\r\n:: another comment\r\n:build\r\nset NAME=world\r\nif defined NAME echo %NAME% \"hello\"\r\nfor %%I in (1 2) do echo %%I\r\necho !NAME!\r\n";
        let side = paint("build.BAT", source);
        for (no, token, category) in [
            (1, "@echo off", "keyword"),
            (2, "REM build café", "comment"),
            (3, ":: another comment", "comment"),
            (4, ":build", "title"),
            (5, "set", "keyword"),
            (5, "NAME", "variable"),
            (6, "if", "keyword"),
            (6, "%NAME%", "variable"),
            (6, "hello", "string"),
            (7, "%%I", "variable"),
            (8, "!NAME!", "variable"),
        ] {
            let line = source.lines().nth(no as usize - 1).unwrap();
            assert!(
                has(&side, no, line, token, category),
                "{no}: {token} {category}: {:?}",
                side.line(no)
            );
        }
    }

    #[test]
    fn powershell_commands_types_comments_and_multiline_strings() {
        let source = "function Get-Greeting {\r\nparam([string]$Name)\r\n<# hidden context\r\nstill a comment café #>\r\n$Count = 42\r\nWrite-Output \"Hello $Name\"\r\n$Text = @\"\r\nhello 世界\r\n\"@\r\nreturn $Text\r\n}\r\n";
        let side = paint("build.PS1", source);
        for (no, token, category) in [
            (1, "function", "keyword"),
            (1, "Get-Greeting", "function"),
            (2, "param", "keyword"),
            (2, "string", "type"),
            (2, "$Name", "variable"),
            (4, "still a comment café", "comment"),
            (5, "42", "number"),
            (6, "Write-Output", "function"),
            (6, "Hello ", "string"),
            (8, "hello 世界", "string"),
            (10, "return", "keyword"),
        ] {
            let line = source.lines().nth(no as usize - 1).unwrap();
            assert!(
                has(&side, no, line, token, category),
                "{no}: {token} {category}: {:?}",
                side.line(no)
            );
        }
        let line = source.lines().nth(7).unwrap();
        let start = line.find('世').unwrap();
        assert_eq!(
            clipped(side.line(8), start..line.len()),
            vec![Span {
                range: 0..6,
                style: 1
            }]
        );
    }

    #[test]
    fn all_mapped_languages_compile_their_queries() {
        for path in [
            "a.rs",
            "a.c",
            "a.cpp",
            "a.cs",
            "a.js",
            "a.ts",
            "a.tsx",
            "a.py",
            "a.json",
            "a.toml",
            "a.yaml",
            "a.sh",
            "a.bat",
            "a.ps1",
            "a.go",
            "a.rb",
            "a.java",
            "a.html",
            "a.css",
            "a.sql",
            "a.md",
            "a.swift",
            "a.scala",
            "a.zig",
            "a.ex",
            "a.graphql",
            "a.proto",
            "CMakeLists.txt",
            "Makefile",
            "a.diff",
        ] {
            let name = language(path).unwrap();
            assert!(configuration(name).is_some(), "{name}");
        }
        assert_eq!(NAMES.len(), crate::desktop_theme::SYNTAX_COLORS.len());
    }

    #[test]
    fn unknown_binary_invalid_large_and_cancelled_sources_fall_back() {
        let mut painter = Painter::default();
        for (name, bytes) in [
            ("a.txt", b"fn main() {}".to_vec()),
            ("a.rs", b"fn\0main".to_vec()),
            ("a.rs", vec![0xff]),
            ("a.rs", vec![b'x'; MAX_SOURCE + 1]),
            ("a.rs", vec![b'x'; MAX_LINE + 1]),
        ] {
            assert!(
                painter
                    .side(name, &bytes, &AtomicUsize::new(0))
                    .spans
                    .is_empty()
            );
        }
        assert!(
            painter
                .side("a.rs", b"fn main() {}", &AtomicUsize::new(1))
                .spans
                .is_empty()
        );
        assert_eq!(language("dir/UPPER.RS"), Some("rust"));
        assert_eq!(language("dir/CMakeLists.txt"), Some("cmake"));
    }

    #[test]
    fn worker_handles_renamed_sides_and_latest_request() {
        let (tx, rx) = async_channel::unbounded();
        let worker = Worker::new(tx);
        worker.submit(
            (1, 1, 0),
            [b"fn old() {}".as_slice().into(), b"plain".as_slice().into()],
            ["old.rs".into(), "new.txt".into()],
        );
        let crate::engine::Msg::DiffSyntax(_, _, _, result) = rx.recv_blocking().unwrap() else {
            panic!("expected syntax")
        };
        assert!(!result.sides[0].spans.is_empty());
        assert!(result.sides[1].spans.is_empty());
        for seq in 2..20 {
            worker.submit(
                (1, seq, 0),
                [Arc::from([]), Arc::from(&b"fn latest() {}"[..])],
                ["a.rs".into(), "a.rs".into()],
            );
        }
        loop {
            let crate::engine::Msg::DiffSyntax(_, seq, _, result) = rx.recv_blocking().unwrap()
            else {
                panic!("expected syntax")
            };
            if seq == 19 {
                assert!(!result.sides[1].spans.is_empty());
                break;
            }
        }
    }

    #[test]
    #[ignore = "component benchmark; run release after builds finish"]
    fn benchmark_syntax() {
        use std::{hint::black_box, time::Instant};
        let rust = (0..1200).map(|i| format!(
            "pub fn item_{i}(value: usize) -> usize {{ let label = \"{}\"; value + {i} }}\n",
            "wide source text ".repeat(12))).collect::<String>();
        let python = (0..1200)
            .map(|i| format!("def item_{i}(value):\n    return value + {i} # café 世界\n"))
            .collect::<String>();
        let json = format!(
            "[{}]",
            (0..1200)
                .map(|i| format!("{{\"id\":{i},\"label\":\"hello\",\"enabled\":true}}"))
                .collect::<Vec<_>>()
                .join(",\n")
        );
        let powershell = (0..600).map(|i| format!(
            "function Get-Item{i} {{ param([string]$Name) $Count = {i}; Write-Output \"Hello $Name\" }}\n"
        )).collect::<String>();
        let batch = (0..600)
            .map(|i| {
                format!(
                    "REM build step {i}\nset NAME=world{i}\nif defined NAME echo \"Hello %NAME%\"\n"
                )
            })
            .collect::<String>();
        let csharp = format!("public class View {{\n{}\n}}", (0..600).map(|i| format!(
            "public int Item{i}(int value) {{ var label = \"Hello 世界\"; return value + {i}; }}\n"
        )).collect::<String>());
        let xaml = format!(
            "<Grid xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\">\n{}\n</Grid>",
            (0..600)
                .map(|i| format!(
                    "<TextBlock x:Name=\"Item{i}\" Grid.Row=\"{i}\" Text=\"{{Binding Name}}\" />\n"
                ))
                .collect::<String>()
        );
        for (path, text) in [
            ("View.cs", csharp),
            ("View.xaml", xaml),
            ("wide.rs", rust),
            ("a.py", python),
            ("a.json", json),
            ("build.ps1", powershell),
            ("build.bat", batch),
            ("large.rs", "let x = 1;\n".repeat(60_000)),
        ] {
            let mut painter = Painter::default();
            for sample in -1..15 {
                let start = Instant::now();
                let side = painter.side(path, black_box(text.as_bytes()), &AtomicUsize::new(0));
                let elapsed = start.elapsed().as_secs_f64() * 1000.;
                let count = side.spans.len();
                let lookup = Instant::now();
                for frame in 0..120 {
                    for row in frame * 7..frame * 7 + 50 {
                        black_box(clipped(side.line(row + 1), 12..100));
                    }
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "path":path,"sample":sample,"bytes":text.len(),"syntaxMs":elapsed,
                        "cached50RowFrameUs":lookup.elapsed().as_secs_f64()*1e6/120.,
                        "spans":count,"retainedBytes":side.spans.capacity()*std::mem::size_of::<Span>()
                            +side.lines.capacity()*std::mem::size_of::<Range<usize>>()
                    })
                );
                if path != "large.rs" {
                    assert!(count > 0);
                } else {
                    assert_eq!(count, 0);
                }
            }
        }
    }
}
