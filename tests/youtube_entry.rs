//! The identity a `YouTube` search result carries into the queue (issue #70,
//! S3): a `youtube://<id>` row that names itself with the body the search
//! already said.

#![cfg(unix)]

use velo::audio::output::Entry;
use velo::youtube::pseudo_path;

#[test]
fn a_youtube_entry_names_itself_and_carries_its_identity() {
    let entry = Entry::Youtube {
        path: pseudo_path("abc"),
        title: "Song".to_owned(),
        uploader: Some("Channel".to_owned()),
        duration: Some(213),
    };

    assert_eq!(entry.label(), "Song");
    assert_eq!(entry.as_path(), Some(pseudo_path("abc").as_path()));
}
