# Refresh diagnostic build

The `0.1.14-refresh-diag.1` prerelease enables the `refresh-diagnostics` Cargo
feature. It records refresh operation timing and numeric counts automatically.
Build with `cargo build --release --features refresh-diagnostics`.

Run the portable executable normally to use existing application settings and
credentials. The default output is
`%LOCALAPPDATA%\git-rustle\diagnostics\refresh-<epoch>-<pid>.jsonl`.
`GIT_RUSTLE_REFRESH_LOG` overrides this with an exact filename; `off` disables
recording. Create separate sessions when comparing behavior. Close the app
normally before collecting the log so its shutdown guard can join the writer.

Create a portable ZIP without launching the app:

```powershell
./scripts/package-refresh-diagnostics.ps1 -Executable target/release/git-rustle.exe -OutputDirectory target/bench/refresh-package
```

The package contains the exact supplied executable, a README and
`Start-Diagnostics.cmd`. A user can run the launcher with a quoted repository
path, such as `Start-Diagnostics.cmd "C:\Work\My Repository"`. It records to a
fresh filename in the package's `Logs` folder and preserves application data
environment variables and credentials. The package manifest records SHA-256.
Distribute it as the `v0.1.14-refresh-diag.1` GitHub prerelease, without marking
it latest. Confirm the supplied binary was built with diagnostics before packaging.

For a useful capture: open the repository, wait for initial loading, click the
Graph's on-screen Refresh button two or three times (waiting for each to finish),
then open Branches and wait for its full listing to finish. Close the application
normally and collect the JSONL file. Do not rely on F5: the current desktop does
not wire the model's F5 shortcut. Record which view felt slow when sharing the log.

## Log contract and privacy

Each UTF-8 JSONL record has `v` (1), increasing writer `seq`, numeric
`session_ms`, `thread` (`ThreadId(N)`), `event` (`begin`, `end`, `event`), static
`name`, nullable `span`, nullable `duration_ms`, nullable static `outcome`, and
`metrics` containing static keys and unsigned integer values. Span begins and
ends share their identifier and name. End records supply duration and outcome.
Concurrent producers can enqueue timestamps out of order; sequence reflects
writer order. Request sequences are numeric metrics rather than repository data.

Do not add paths, URLs, branch names, object IDs, contents, tokens, dynamic
labels or raw errors. Use static outcome categories. Fixture validation should
pass distinctive private values as `-ForbiddenSentinel`, covering each private
category exercised by the scenario. This is evidence for those fixture values;
it is not proof against every possible future leak. Review logs before sharing.

The bounded asynchronous queue drops records instead of blocking application
work. `session_end.metrics.dropped_events` reports loss. The writer flushes every
250 ms and stops at 64 MiB, emitting `log_limit_reached` when possible. Forced
termination or an I/O failure can omit shutdown records. Treat dropped, limited
or interrupted sessions as incomplete: missing spans do not establish that an
operation never ran.

HTTP header-wait time combines connection/DNS/TLS/server waiting; it does not
separate those transport phases. Spans can overlap and must not be summed as
wall-clock latency. Request numbers label UI refresh windows; unrelated periodic
work may overlap them. A next-frame callback is a GPUI frame boundary, not proof
of physical presentation. Writing to a slow or stalled filesystem can delay log
shutdown; use a local writable folder. The size-limit marker means truncation,
including queued records not represented by the queue-overflow count.

## Automated validation

```powershell
./scripts/test-refresh-diagnostics.ps1 -LogPath target/bench/run/candidate/refresh.jsonl -RequiredName session_end -ForbiddenSentinel 'PRIVATE_FIXTURE_PATH','PRIVATE_FIXTURE_BRANCH'
```

The verifier checks schema, ordered sequences, numeric measurements, static
string syntax, begin/end matching and orderly completion. `-AllowIncomplete`
permits inspection of lost or interrupted sessions while still checking record
shape and privacy sentinels; it does not certify completeness. Use logger unit
tests to validate queue overflow, size limits, disable behavior and shutdown.

The graph refresh benchmark clears inherited `GIT_*` variables. Its diagnostics
option must set the candidate's exact log path **after** creating each sample
directory and set baseline logging to `off`. Save and restore the variable even
when it was initially absent. Keep logs beside each sample's state and timing
evidence under ignored `target/bench/<run>`. Measure default logging and disabled
logging separately with the same preserved baseline and fixture.

Follow [the benchmark workflow](benchmarking.md) and [AGENTS.md](../AGENTS.md).
Native scenarios use the private, non-current desktop helper and await worker
results and actual paints. Never launch this packaging helper as a UI test or
fall back to the user's desktop. Record behavioral, performance and any PNG
review findings separately; automated paints do not establish physical input,
desktop composition, scanout or human UX judgment.
