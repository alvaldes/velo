#!/usr/bin/env bash
# Cuts a release of velo from the Conventional Commits since the last `v*` tag:
# bumps the version in Cargo.toml, refreshes Cargo.lock, regenerates
# CHANGELOG.md, commits `chore(release): vX.Y.Z` and tags it `vX.Y.Z`.
#
# Run it once per change delivered to main (a merged feature or a fix), never
# per work-in-progress commit. Pushing the tag runs the release workflow.
#
#   feat → minor   fix, perf → patch   breaking → minor while 0.x
#
# Needs git-cliff (`brew install git-cliff`).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

fail() {
  echo "release: $*" >&2
  exit 1
}

command -v git-cliff >/dev/null || fail "git-cliff is missing (brew install git-cliff)"

branch=$(git branch --show-current)
[[ "$branch" == main ]] || fail "run on main, not on '$branch'"

# Only the files this script writes have to be clean; unrelated work in the
# tree is left alone and never staged.
for file in Cargo.toml Cargo.lock CHANGELOG.md; do
  git diff --quiet HEAD -- "$file" 2>/dev/null || fail "$file has uncommitted changes"
done

last=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null) ||
  fail "no v* tag to count from"

# Read the log whole first: `grep -q` stops at the first match, and under
# pipefail the SIGPIPE it leaves `git log` with would read as "no match".
messages=$(git log "$last..HEAD" --format='%s%n%b')
if ! grep -Eq '^(feat|fix|perf)(\([^)]*\))?!?:|^BREAKING CHANGE' <<<"$messages"; then
  echo "release: nothing to release since $last (no feat, fix or perf commits)"
  exit 0
fi

next=$(git cliff --bumped-version 2>/dev/null)
version=${next#v}
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "git-cliff proposed '$next'"
git rev-parse -q --verify "refs/tags/$next" >/dev/null && fail "$next is already tagged"

# The package version is the first `version =` line of Cargo.toml.
perl -pi -e 'if (!$done && s/^version = "[^"]*"/version = "'"$version"'"/) { $done = 1 }' Cargo.toml
cargo update --workspace --quiet
git cliff --tag "$next" --output CHANGELOG.md 2>/dev/null

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit --quiet -m "chore(release): $next"
git tag -a "$next" -m "velo $next"

echo "release: $last → $next, committed and tagged"
echo "release: publish with: git push --follow-tags"
