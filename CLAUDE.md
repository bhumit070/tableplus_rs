# CLAUDE.md

Instructions for Claude Code when working in this repository.

## Branching

- Never commit directly to `main`. Every task (feature, fix, refactor, docs change — anything) starts by creating a new branch off `main`.
- Name branches descriptively, e.g. `feat/autocomplete-popup`, `fix/mssql-cancel`, `docs/update-readme`.
- Land changes on `main` only by merging that branch (open a PR and merge it, or merge locally if the user asked for that explicitly) — never `git push origin <branch>:main` or an equivalent direct push.
- Always **squash and merge** (`gh pr merge --squash --delete-branch`, or the "Squash and merge" button on GitHub) — never a regular merge commit or rebase-merge. `main`'s history should be one commit per PR.
- The one exception is `.github/workflows/release.yml`'s own `version` job: it pushes a `vX.Y.Z` *tag* (not a commit, and not to `main`) after a push lands on `main`, purely to drive the release build. That's CI bookkeeping, not a code change, and isn't subject to this rule.

## Releases

- `.github/workflows/release.yml` runs on every push to `main`: it tags the commit `vX.Y.Z` (bumping the patch version from the latest existing tag, starting at `v0.0.1`), builds a `.dmg` (macOS, universal x86_64+aarch64), a `.exe` (Windows), and a `.tar.gz` (Linux x86_64), and publishes them as a GitHub Release.
- The release version (the git tag) is intentionally decoupled from the `version` in the root `Cargo.toml`. Bump `Cargo.toml`'s version yourself, in a normal branch+merge, whenever you want the compiled-in crate version to move — don't try to make the workflow do it, and don't hand-edit it just because a release happened.
- macOS builds are unsigned/unnotarized (no Apple Developer certificate configured) — first launch needs a Gatekeeper right-click-Open.
