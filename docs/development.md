# Developing Git Rustle

## Build and test

Follow the [Windows build instructions](../README.md#build-from-source). Use
the `fast` profile for local iteration and `release` for shipping and performance
comparisons. A default debug build loads shader files from the build machine’s
Cargo registry. Copying that executable alone can produce an
`Error creating DirectWriteTextSystem` on another PC.

```powershell
cargo fmt --check
cargo test --release --locked
cargo clippy --all-targets --locked -- -D warnings
```

Before a performance-sensitive change, preserve a release executable and
measure the affected operation. For UI changes, exercise the real action on a
private desktop and inspect window screenshots in a separate run. See the
[benchmark workflow](benchmarking.md) and [screenshot guide](app-screenshots.md).

## Update the Windows icon

Edit `assets/app-icon.png`, install Pillow, and run
`python scripts/generate-app-icon.py` to regenerate the multi-size ICO. For
source artwork with a white matte, use
`python scripts/remove-icon-matte.py INPUT.png OUTPUT.png` first.

## Publish a release

1. Update the package version in `Cargo.toml` and `Cargo.lock` and commit it.
2. Push the matching tag (for example, `v0.2.0` for version `0.2.0`). A
   prerelease suffix produces a prerelease.
3. The [Windows release workflow](../.github/workflows/windows-release.yml)
   builds and tests the x64 executable, checks its version, and publishes
   `git-rustle-x86_64-pc-windows-msvc.exe` and `SHA256SUMS` together. A
   failed draft upload can be retried. To replace an already published binary,
   release a new version.

A manual workflow run on a branch produces an Actions artifact retained for 90
days. The in-app updater uses stable GitHub Releases and verifies the asset
against GitHub’s SHA-256 digest. Binaries do not carry an Authenticode signature.
