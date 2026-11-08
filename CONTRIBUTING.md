# Contributing to velo

velo is a single Rust binary: a terminal music player that indexes a local
library, plays it gaplessly and talks to Subsonic. No daemon, no server, no
database of its own beyond a SQLite index of the files you have.

## Getting set up

The toolchain is pinned in `.mise.toml`, so `mise install` is the shortest path.
Anything with `rustc` 1.88 or newer works too.

Platform dependencies, needed to build and to run the tests:

| Platform | What you need | Why |
|:--|:--|:--|
| Linux | `libasound2-dev`, `cmake`, a C compiler | `cpal` needs ALSA headers; `audiopus_sys` builds libopus from source |
| macOS | Xcode command line tools, `cmake` | CoreAudio; libopus is built by `cmake` |

Releases are built for four targets by `cross`, and its container does **not**
ship the ALSA headers or `cmake`: that is what `Cross.toml` is for, and its
`pre-build` installs them. A comment in `release.yml` once claimed the container
already had them, and a release failed because of it
([run 42519251288](https://github.com/alvaldes/velo/actions/runs/42519251288)) —
so the claim lives here, next to the file that resolves it, instead of in a
second version of the story that can go stale again.

## The commands we run

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

All three run in CI on Linux and macOS, and a change that fails any of them is
not ready. `cargo test` is expected to pass offline: the suites use local
fixtures and hand-written loopback servers, never the real network.

## What a good change looks like

- **Behaviour first, then the code.** For anything with a runnable test, write
  the failing test and see it fail for the right reason before making it pass. A
  test that cannot fail proves nothing.
- **Two tests matter more than ten.** One that pins the rule you just added, and
  one that pins the behaviour you could have broken. Tests that assert on a
  build's current capabilities rot the day the build changes; assert the rule.
- **Small slices.** Around 400 changed lines is a review, 1500 is a rubber stamp.
  One behaviour per commit; tests and docs live in the same commit as the code.
- **Say what could go wrong.** If your change touches stored data, credentials,
  a public contract, concurrency or the audio path, say so in the pull request.
  That is where a reviewer spends their attention.
- **Document what a user sees.** A new command, flag, key or message goes into
  `README.md` and the in-app help card in the same change.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/), written for a
reader of the release notes, because `CHANGELOG.md` is generated from them.
`docs(odd)` commits touch the task logbooks under `odd/` and never reach the
changelog.

No AI attribution in commits, and no generated-by trailers.

## Versioning and releases

Do not edit the version or the changelog by hand. A delivery ends with:

```sh
scripts/release.sh
```

It reads the commits since the last `v*` tag, decides the next version
(`feat` → minor, `fix`/`perf` → patch, breaking → minor while 0.x), updates
`Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`, commits `chore(release): vX.Y.Z`
and tags it. Pushing the tag (`git push --follow-tags`) runs the release
workflow, which builds the binaries.

## How the work is tracked

Non-trivial work gets a logbook in `odd/tasks/<feature>.md`: the specs it must
satisfy, the tasks, and a log of decisions and corrections with the evidence
behind them. It is the document a reviewer reads to know *why*, and it makes a
resumed session cheap. Small fixes do not need one.

## Reporting a problem

Use the issue templates. A bug report that includes your platform, your terminal
and the output of `velo --info` is worth ten without them.
