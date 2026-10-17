# Diff syntax highlighting

The diff becomes readable as soon as its existing file worker finishes. A separate,
lazily started worker colors complete old/new source texts with Tree-sitter and
delivers an immutable span table. View, request and file identities guard delivery;
switching files cannot apply an old result or reset selection/scroll position.
The worker retains one pending request, cancels superseded work, and caches
compiled queries. The current/parked diff retains its spans; reopening a file
computes them again. No source-content cache is persisted to disk.

The GPUI renderer reads spans only for visible lines and clips their UTF-8 byte
ranges for wrapped fragments. Each line is shaped with its original uniform font run; cached glyphs receive
colors at paint time. This avoids GPUI 0.2 splitting platform shaping at every
color boundary and preserves font metrics, ligatures and existing
change/selection backgrounds. Tabs and CRLF use the same decoder as the
diff. Parsing includes collapsed context, so multiline comments and strings do
not restart at hunk boundaries. Each renamed side uses its own filename.

## Coverage and limits

The bundled gpui-component Tree-sitter grammars cover Rust, C/C++, C#, JavaScript
(including JSX), TypeScript/TSX, Python, JSON/JSONC, TOML, YAML, Bash, Go, Ruby,
Java, HTML, CSS, SQL, Markdown, Swift, Scala, Zig, Elixir, GraphQL, Protocol
Buffers, CMake, Make and patch files. Detection uses known extensions plus
Makefile/GNUmakefile, CMakeLists.txt, Gemfile/Rakefile and Bash dotfiles.

This is syntax coloring, not semantic highlighting or language-server analysis.
Only the primary language is parsed: embedded JavaScript in HTML, Markdown fenced
code and other language injections do not get separate parsers. Unsupported
extensions, binary data and invalid UTF-8 remain plain text.

Each side is limited to 512 KiB of input, 1 MiB after tab expansion, 16 KiB per
source line, and 100,000 stored spans. A 150 ms watchdog requests cancellation
across both sides. Tree-sitter checks cancellation cooperatively, so this is a
work budget, not a hard real-time deadline; compiling a language's first query
cannot be interrupted. A side that fails or exceeds a limit returns plain text.
All of this work runs away from the diff worker and UI thread.

## Performance verification

Follow [benchmarking.md](benchmarking.md), preserving an executable before edits
and completing release tests, Clippy and builds before any measurements.
Use an unlocked, visible desktop and run suites sequentially.

Create fresh fixtures inside the ignored run directory:

~~~powershell
./scripts/new-syntax-fixture.ps1 -Path (Join-Path $benchRun 'source-fixture')
./scripts/new-syntax-fixture.ps1 -Path (Join-Path $benchRun 'small-fixture') -Lines 60 -Width 2
./scripts/new-scroll-fixture.ps1 -Path (Join-Path $benchRun 'plain-fixture')
~~~

Compare the preserved baseline with the candidate using 15 alternating launches
after one discarded warm-up per executable/scenario:

~~~powershell
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baselineExe -NativeBaseline -NativeCandidate -RejectCompetingBuilds -Samples 15 -OutputDirectory (Join-Path $benchRun 'startup')
./scripts/benchmark-desktop.ps1 -Repository (Join-Path $benchRun 'source-fixture') -Baseline $baselineExe -RequireSyntax -RejectCompetingBuilds -Samples 15 -Scenarios @('diff','diff-scroll','diff-pan','wrap-scroll') -OutputDirectory (Join-Path $benchRun 'source')
./scripts/benchmark-desktop.ps1 -Repository (Join-Path $benchRun 'small-fixture') -Baseline $baselineExe -RequireSyntax -RejectCompetingBuilds -Samples 15 -Scenarios @('diff') -OutputDirectory (Join-Path $benchRun 'small')
./scripts/benchmark-desktop.ps1 -Repository (Join-Path $benchRun 'plain-fixture') -Baseline $baselineExe -RejectCompetingBuilds -Samples 15 -Scenarios @('diff','diff-scroll') -OutputDirectory (Join-Path $benchRun 'plain')
cargo test --release diff::syntax::tests::benchmark_syntax -- --ignored --exact --nocapture
~~~

The existing workerMs boundary still measures plain diff delivery. syntaxReadyMs
measures file request through color installation (including queueing, first-use
query compilation, normalization, parsing and delivery). scenarioFrameAfterRequestMs
also includes the next qualifying render callback. The baseline has no syntax
metric. Candidate diff/scroll scenarios wait for syntax completion, and
RequireSyntax rejects an accidentally uncolored source fixture.

Each scrolling launch saves 120 measured next-frame intervals. They include
scheduling, layout and paint submission, not physical input latency or scanout.
The component harness prints one first-use sample (sample -1, retained separately)
and 15 warm samples for Rust, Python, JSON and large-file fallback. It separately
times cached range lookup/clipping for 50 rows; this excludes shaping and paint.
Component timings are candidate-only. Native baseline/candidate comparisons
establish the end-to-end regression budget.

Keep all raw logs, samples, source/executable hashes, sizes and a report under
target/bench/<run>. Inspect the native UI for foreground contrast, selection,
copying, wrapping and horizontal scrolling; model dumps alone cannot verify pixels.


Use scripts/summarize-syntax-benchmark.ps1 -Run $benchRun to summarize the standard
startup/source/small/plain output directories. RejectCompetingBuilds aborts a native
run if Rust/C/C++ compiler or Cargo processes appear at its launch boundaries;
retain those logs as rejected data and repeat after other work finishes.
