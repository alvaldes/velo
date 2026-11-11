#!/bin/sh
# The gate: exactly what CI runs, in the same order and with the same flags.
#
# It exists because the two drifted. CI runs clippy with `-D warnings`, so a
# warning is a failure there; running `cargo clippy` by hand prints the same
# warning in yellow and calls it "pre-existing", and the red arrives only after
# the push. One command, one set of flags, no room for that.
#
# `.github/workflows/ci.yml` calls this script instead of repeating the steps,
# so the workflow and a local run cannot disagree again.
set -eu

cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
