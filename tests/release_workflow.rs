//! The release workflow's Homebrew step (delivery): the tap's formula carries
//! the checksums of one release, so the release that publishes them is the one
//! that points the formula at them.
//!
//! These pin the two ways that step went wrong before it ever ran in anger —
//! parsing the dispatch's stdout, which a runner does not get, and failing a
//! release whose archives are already public — plus the ref that keeps a
//! fine-grained token from needing a permission it is not asked for.

use std::fs;

/// The workflow file, as the release job reads it.
fn workflow() -> String {
    fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/release.yml"
    ))
    .expect("the release workflow reads")
}

/// The tap step's own block: from its name to the next step's name.
fn tap_step() -> String {
    let workflow = workflow();
    let start = workflow
        .find("Point the tap's formula at this release")
        .expect("the release points the tap at itself");
    let rest = &workflow[start..];
    let end = rest.find("\n      - name:").unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn the_tap_step_never_parses_the_dispatch_stdout() {
    let step = tap_step();
    assert!(
        !step.contains("$(gh workflow run"),
        "`gh workflow run` prints nothing when stdout is a pipe, which is how a \
         runner calls it: the id must never come from that output.\n{step}"
    );
    assert!(
        !step.contains("${url##*/}"),
        "there is no url to take an id from on a runner"
    );
    assert!(
        step.contains("comm -13"),
        "the dispatched run is the one whose id is new, never the newest run"
    );
}

#[test]
fn the_tap_dispatch_pins_its_ref() {
    let step = tap_step();
    assert!(
        step.contains("-r main"),
        "without --ref gh asks GraphQL for the default branch first, which a \
         fine-grained token needs `Contents: read` for"
    );
    assert!(
        step.contains("update.yml --repo \"$tap\" -r main -f tag=\"$TAG\""),
        "the dispatch names the workflow, the repository, the ref and the tag: {step}"
    );
}

#[test]
fn a_tap_that_refuses_never_fails_the_release() {
    let step = tap_step();
    assert!(
        !step.contains("exit 1"),
        "by the time this step runs the archives are published; the tap's cron \
         is the fallback and a red release would be a lie"
    );
    assert!(
        step.contains("::warning title=Tap not updated"),
        "the drift is still visible as an annotation"
    );
    assert!(
        step.contains("gh run view"),
        "the run is polled with a bound, never watched without one"
    );
    assert!(
        !step
            .lines()
            .any(|line| line.trim_start().starts_with("gh run watch")),
        "`gh run watch` has no timeout: a tap run that never starts would hold \
         the release job"
    );
}

#[test]
fn the_missing_token_guard_comes_before_the_first_gh_call() {
    let step = tap_step();
    let guard = step.find("TAP_TOKEN is missing").expect("the guard");
    let first_call = step.find("gh ").expect("a gh call");
    assert!(
        guard < first_call,
        "the guard must precede every gh call, or a missing token fails the step"
    );
    assert!(
        step[guard..first_call].contains("exit 0"),
        "a missing token warns and stops, it never fails the release"
    );
}
