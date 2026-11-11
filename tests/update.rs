//! The check you ask for (issue #69): `velo update` in the shell. The
//! verdict text and the install route are pure functions; the check itself
//! runs over the injected transport seam — no test touches the network.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use velo::radio::client::{Error as TransportError, Transport};
use velo::update::{
    InstallRoute, RELEASES_URL, fetch_latest_tag, install_route, manual_check, manual_report,
};

/// A transport that answers one canned body — or fails with one canned
/// reason — and records how many requests it saw, the same seam the
/// automatic check's worker runs on.
struct Canned {
    answer: Result<String, TransportError>,
    asked: AtomicUsize,
}

impl Canned {
    fn with(body: &str) -> Self {
        Self {
            answer: Ok(body.to_owned()),
            asked: AtomicUsize::new(0),
        }
    }

    fn failing(detail: &str) -> Self {
        Self {
            answer: Err(TransportError::Unreachable {
                detail: detail.to_owned(),
            }),
            asked: AtomicUsize::new(0),
        }
    }
}

impl Transport for Canned {
    fn get(&self, url: &str) -> Result<String, TransportError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            url, RELEASES_URL,
            "the check asks the releases endpoint, nothing else"
        );
        self.answer.clone()
    }
}

// ------------------------------------------- the install route, from the path

#[test]
fn a_homebrew_prefix_says_brew() {
    for path in [
        "/opt/homebrew/bin/velo",
        "/opt/homebrew/Cellar/velo/0.47.1/bin/velo",
        "/usr/local/Cellar/velo/0.47.1/bin/velo",
    ] {
        assert_eq!(
            install_route(Path::new(path)),
            InstallRoute::Homebrew,
            "{path}"
        );
    }
}

#[test]
fn any_other_path_says_the_release_archive() {
    for path in [
        "/usr/local/bin/velo",
        "/usr/bin/velo",
        "/home/alice/.cargo/bin/velo",
        "velo",
    ] {
        assert_eq!(
            install_route(Path::new(path)),
            InstallRoute::ReleaseArchive,
            "{path}"
        );
    }
}

#[test]
fn each_route_names_its_own_way_to_get_it() {
    assert_eq!(InstallRoute::Homebrew.advice(), "brew upgrade velo");
    assert!(
        InstallRoute::ReleaseArchive
            .advice()
            .contains("github.com/alvaldes/velo/releases"),
        "the archive route names the releases page: {}",
        InstallRoute::ReleaseArchive.advice()
    );
}

// ---------------------------------------------------------- the verdict text

#[test]
fn a_newer_version_says_so_and_how_to_get_it() {
    let report = manual_report("0.47.1", "v0.48.0", InstallRoute::Homebrew);
    assert!(report.contains("0.48.0 is available"), "{report}");
    assert!(report.contains("you have 0.47.1"), "{report}");
    assert!(report.contains("brew upgrade velo"), "{report}");
}

#[test]
fn the_release_archive_route_names_the_page() {
    let report = manual_report("0.47.1", "v0.48.0", InstallRoute::ReleaseArchive);
    assert!(report.contains("0.48.0 is available"), "{report}");
    assert!(
        report.contains("github.com/alvaldes/velo/releases"),
        "{report}"
    );
}

#[test]
fn the_newest_release_is_named_even_up_to_date() {
    let report = manual_report("0.47.1", "v0.47.1", InstallRoute::Homebrew);
    assert!(report.contains("0.47.1"), "{report}");
    assert!(report.contains("up to date"), "{report}");
}

#[test]
fn an_equal_version_is_up_to_date_and_claims_no_update() {
    let report = manual_report("0.47.1", "v0.47.1", InstallRoute::Homebrew);
    assert!(report.contains("up to date"), "{report}");
    assert!(
        !report.contains("available"),
        "an equal version is not news: {report}"
    );
}

#[test]
fn a_running_build_ahead_of_the_releases_claims_no_update() {
    let report = manual_report("0.48.0", "v0.47.1", InstallRoute::ReleaseArchive);
    assert!(report.contains("up to date"), "{report}");
    assert!(
        report.contains("0.47.1"),
        "the newest release is named anyway: {report}"
    );
    assert!(
        !report.contains("available"),
        "older is never claimed as news: {report}"
    );
}

// ------------------------------------- the check itself, over the seam

#[test]
fn the_manual_check_reports_the_verdict() {
    let transport = Canned::with(r#"{"tag_name":"v9.9.9"}"#);
    let report = manual_check(&transport, InstallRoute::Homebrew).expect("a body that reads");
    assert!(report.contains("9.9.9 is available"), "{report}");
    assert!(report.contains("brew upgrade velo"), "{report}");
    assert_eq!(transport.asked.load(Ordering::SeqCst), 1, "one request");
}

#[test]
fn a_failed_transport_is_one_line_of_reason_and_no_report() {
    let transport = Canned::failing("the releases page refused the request");
    let error = manual_check(&transport, InstallRoute::Homebrew)
        .expect_err("a failed request is an error, said in one line");
    assert!(!error.contains('\n'), "one line: {error:?}");
    assert!(
        error.contains("the releases page refused the request"),
        "the reason names what went wrong: {error}"
    );
}

#[test]
fn a_body_that_is_not_the_json_is_one_line_of_reason() {
    let transport = Canned::with("not json at all");
    let error = manual_check(&transport, InstallRoute::ReleaseArchive)
        .expect_err("a bad body is an error, said in one line");
    assert!(!error.contains('\n'), "one line: {error:?}");
    assert!(
        !error.contains("not json at all"),
        "the reason is velo's sentence, not the body: {error}"
    );
}

// --------------- the automatic check's silence, unchanged by the refactor

#[test]
fn the_automatic_check_stays_silent_on_every_answer() {
    let failing = Canned::failing("refused");
    assert_eq!(
        fetch_latest_tag(&failing),
        None,
        "a failed request is silent"
    );
    let broken = Canned::with("not json");
    assert_eq!(fetch_latest_tag(&broken), None, "a bad body is silent");
    let good = Canned::with(r#"{"tag_name":"v9.9.9"}"#);
    assert_eq!(fetch_latest_tag(&good), Some("v9.9.9".to_owned()));
}
