# velo — working rules

## Every delivered change bumps the version

A change is delivered when it lands on `main`: a merged feature branch, or a
`feat`/`fix`/`perf` commit made directly on `main`. Every delivery ends with:

```sh
scripts/release.sh
```

It reads the Conventional Commits since the last `v*` tag and decides the
next version (`feat` → minor, `fix`/`perf` → patch, breaking → minor while
0.x), updates `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`, commits
`chore(release): vX.Y.Z` and tags `vX.Y.Z`. Do not edit the version or the
changelog by hand, and do not run it per work-in-progress commit: one release
per delivery. A delivery with only `docs`, `test`, `refactor` or `chore`
commits releases nothing, and the script says so.

Pushing the tag (`git push --follow-tags`) runs `.github/workflows/release.yml`.

## Before pushing

Run `scripts/check.sh` — it is the same three steps CI runs (`fmt --check`,
`clippy -D warnings`, `test`). CI turns clippy *warnings* into failures, so a
plain `cargo clippy` passing means nothing there.

## Commits

Conventional Commits, no AI attribution. The changelog is generated from them,
so the subject is written for a reader of the release notes. `docs(odd)`
commits (the task logbooks under `odd/`) never reach the changelog.
