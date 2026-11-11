//! The shapes a list row's text takes, in one place (issue #71).
//!
//! Every view already draws its rows through [`super::list_row`]: the
//! divergence was never the drawing, it was the text handed to it. The radio
//! view spelled the favourite marker `★ ` in its favourites section and `★`
//! in the catalog, so the same station read two ways and only one of them was
//! the intended one. Each shape below has exactly one owner, so a second
//! spelling cannot appear in one list without the other following.
//!
//! What stays deliberately different, and is therefore a parameter rather
//! than something these functions decide: the bracketed value a station shows
//! (a favourite's codec, `[MP3]`, against a catalog station's bitrate,
//! `[128k]`, because the store keeps no bitrate for a favourite), and whether
//! a row is marked (`★`) or refused (`✗`) at all.

/// The favourite marker. Every list puts one space after it.
const STAR: &str = "★";

/// The marker for a station that cannot play, and the space after it.
const CROSS: &str = "✗ ";

/// `★ Name` — the one way a row says it is a favourite, whether the row is a
/// track, a station, a place or a playlist.
#[must_use]
pub fn starred(name: &str) -> String {
    format!("{STAR} {name}")
}

/// The tail that follows a station's name: the bracketed value and the
/// country code, each only when it is known, leading with the space that
/// separates them from the name. `None` for the bracket when the store knows
/// nothing to put there; passing `Some("")` is the same as passing `None`.
#[must_use]
pub fn tail(bracket: Option<&str>, country: &str) -> String {
    use std::fmt::Write as _;

    let mut tail = String::new();
    if let Some(value) = bracket.filter(|value| !value.is_empty()) {
        let _ = write!(tail, " [{value}]");
    }
    if !country.is_empty() {
        let _ = write!(tail, " · {country}");
    }
    tail
}

/// `★ ✗ Name [128k] · DE` — identity first (the star), then state (the
/// cross), then the name and its [`tail`]. A kept station that cannot play is
/// both things at once: the star says what the row is, the cross what it
/// cannot do, and the order reads left to right in that order.
#[must_use]
pub fn station_label(favourite: bool, refused: bool, name: &str, tail: &str) -> String {
    let mut label = String::new();
    if favourite {
        label.push_str(STAR);
        label.push(' ');
    }
    if refused {
        label.push_str(CROSS);
    }
    label.push_str(name);
    label.push_str(tail);
    label
}

/// `● N listening now` — the listener count of a channel, as the live
/// row's dim fact (issue #68): it belongs to the row that plays the
/// channel live, not to the song list. One owner, so the `●` is never
/// spelled twice.
#[must_use]
pub fn listening(count: u32) -> String {
    format!("● {count} listening now")
}

/// `N listening now` — the same fact without the marker, for a row that has
/// already spent its bullet on the separator before it (`120 tracks · 17
/// listening now`). The live row keeps [`listening`]: there the bullet is the
/// thing that says live.
#[must_use]
pub fn listeners(count: u32) -> String {
    format!("{count} listening now")
}

/// `Name — live`, or `★ Name — live` when the channel's stream is marked —
/// the one way a row says it plays the channel itself, not one of its songs
/// (issue #68): the channel's live stream, through the same path a
/// directory station takes. The mark is [`starred`], the one marker every
/// marked row wears.
#[must_use]
pub fn live(name: &str, favourite: bool) -> String {
    let label = if favourite {
        starred(name)
    } else {
        name.to_owned()
    };
    format!("{label} — live")
}

/// The dim note after a row's name: the parts that are known, joined by
/// ` · `. A part that is empty is left out, so a row with an unknown artist
/// reads `Harbor`, never ` · Harbor`.
#[must_use]
pub fn note(parts: &[&str]) -> String {
    let known: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !part.is_empty())
        .collect();
    known.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::{listening, live, note, starred, station_label, tail};

    #[test]
    fn the_star_carries_one_space_in_every_shape() {
        assert_eq!(starred("Nova"), "★ Nova");
        assert_eq!(
            station_label(true, false, "Nova", ""),
            "★ Nova",
            "a station and a track spell the marker the same way"
        );
    }

    #[test]
    fn the_cross_comes_after_the_star_and_before_the_name() {
        assert_eq!(station_label(true, true, "Jazz", ""), "★ ✗ Jazz");
        assert_eq!(station_label(false, true, "Jazz", ""), "✗ Jazz");
        assert_eq!(station_label(false, false, "Jazz", ""), "Jazz");
    }

    #[test]
    fn the_tail_carries_what_the_store_knows() {
        assert_eq!(tail(Some("MP3"), "NO"), " [MP3] · NO");
        assert_eq!(tail(Some("128k"), ""), " [128k]");
        assert_eq!(tail(None, "DE"), " · DE");
        assert_eq!(tail(None, ""), "");
        assert_eq!(
            tail(Some(""), "DE"),
            " · DE",
            "an empty bracket is no bracket"
        );
    }

    #[test]
    fn the_live_row_names_the_channel_and_its_listeners() {
        assert_eq!(live("Lofi", false), "Lofi — live");
        assert_eq!(
            live("Lofi", true),
            "★ Lofi — live",
            "a marked live row wears the one star every marked row wears"
        );
        assert_eq!(listening(47), "● 47 listening now");
    }

    #[test]
    fn a_note_leaves_out_the_parts_that_are_unknown() {
        assert_eq!(note(&["Tide", "Harbor"]), "Tide · Harbor");
        assert_eq!(note(&["", "Harbor"]), "Harbor");
        assert_eq!(note(&["Tide", ""]), "Tide");
        assert_eq!(note(&["", ""]), "");
        assert_eq!(note(&[]), "");
    }
}
