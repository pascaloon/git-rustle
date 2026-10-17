# Performance expectations

Performance is a primary product requirement. Benchmark notable changes using
[the benchmark workflow](docs/benchmarking.md), preserving a baseline executable
before changing code. Measure the affected operation as well as startup when
relevant; startup alone cannot establish that remote previews or scrolling stay
fast. Record raw samples and a Markdown report with results and limitations under
the ignored `target/bench/<run>` directory. Keep generated benchmark artifacts out
of version control; commit only benchmark instructions, scripts, and harness code.

Documentation-only changes do not require performance runs. Previous measurements
are not substitutes for measuring a new implementation.
