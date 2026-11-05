# Releases and CI

Two GitHub Actions workflows live in `.github/workflows/`.

## Releases (`release.yml`)

Runs on every push to `main` that changes Rust code (`src/`,
`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` or the workflow itself),
and can be started by hand (*Run workflow*). Docs-only changes don't make a
release.

```mermaid
flowchart LR
    V[version<br/>pick next tag] --> B
    T[test<br/>cargo test on macOS] --> B
    subgraph B[build, one job per platform]
        M1[macOS Apple Silicon]
        M2[macOS Intel]
        L[Linux x86_64]
        W[Windows x86_64]
    end
    B --> R[release<br/>publish GitHub Release]
```

1. **version** picks the next version. MAJOR.MINOR come from `Cargo.toml`.
   PATCH is one more than the latest `vMAJOR.MINOR.*` tag, starting at 0.
   To start a new series, bump `Cargo.toml` to e.g. `0.2.0` and the next
   release is `v0.2.0`.
2. **test** runs `cargo test --locked`. If tests fail, nothing is built.
3. **build** compiles a release binary per platform and packages it:

    | Platform | Files | How |
    | --- | --- | --- |
    | macOS (arm64, x86_64) | `.dmg` | `Tabula.app` bundle (Info.plist with the version), ad-hoc code signature (required to run on Apple Silicon), then `hdiutil` makes a disk image with an Applications shortcut. |
    | Linux x86_64 | `.deb`, `.tar.gz` | `cargo deb` with the metadata in `Cargo.toml`, which installs `/usr/bin/tabula` and a desktop entry and depends on the Vulkan loader. The tarball holds the binary and README. Built on Ubuntu 22.04 so it runs on older glibc too. |
    | Windows x86_64 | `.msi`, `.exe` | WiX 5 builds the installer from `packaging/windows/tabula.wxs`: per-machine install in Program Files, Start menu shortcut, upgrades replace older versions. The bare `.exe` runs without installing. |

    The build jobs use `fail-fast: false`: one platform failing doesn't
    stop the others.

4. **release** publishes a GitHub Release (`vX.Y.Z`) with every file that
   was built, notes on installing each, and the generated changelog. If
   any expected file is missing, the notes list it.

Only one release runs at a time (`concurrency: release`), so two pushes
can't claim the same version.

### Not signed yet

- **macOS:** the app is ad-hoc signed but not notarized, so Gatekeeper
  asks on first launch (right-click → Open). Notarizing needs an Apple
  Developer ID.
- **Windows:** the `.exe` and `.msi` aren't code-signed, so SmartScreen may
  warn (More info → Run anyway). Signing needs a code-signing certificate.

## Documentation site (`docs.yml`)

Runs on every push to `main` that changes `docs/`, `mkdocs.yml` or the
workflow itself, and by hand.

1. Installs MkDocs Material from `docs/requirements.txt`.
2. `mkdocs build --strict`: broken internal links or a bad config fail the
   build instead of publishing a broken site.
3. Uploads the `site/` folder and deploys it to GitHub Pages:
   <https://bhumit070.github.io/tableplus_rs/>.

GitHub Pages is set to **GitHub Actions** as its source (repository
*Settings → Pages*).
