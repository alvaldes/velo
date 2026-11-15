//! The first screen (issue #3): the sources velo can play, one row each,
//! with the cursor already on the first one. The list is fixed — entries
//! that are compiled in — so this module holds the cursor and the labels,
//! nothing else.

/// A source the player can open: what the first screen names, one row each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// The local library index, with its four views inside it.
    #[default]
    Library,
    /// Live radio: the directory, the country, the user's own stations.
    Radio,
    /// `YouTube`, played through `yt-dlp` (issue #70): a row honest about
    /// the tool it needs — its fact says whether the launch found one, and
    /// the screen behind it searches, queues and plays.
    Youtube,
    /// The history's entries (issue #5), newest first.
    RecentlyPlayed,
    /// The saved store's favourites (issue #7): the tracks the user marked.
    Favourites,
    /// The saved store's playlists (issue #4): the named lists that outlive
    /// the process.
    Playlists,
}

impl Source {
    /// Every source, in the order the first screen shows them (S2, revised by
    /// the author: Radio leads, because a fresh install reaches for a station
    /// before it has a folder to index, and the local library is the second
    /// row rather than the first). `YouTube` (issue #70) sits directly beside
    /// Radio: the two sources that go looking outside the machine.
    pub const ALL: [Self; 6] = [
        Self::Radio,
        Self::Youtube,
        Self::Library,
        Self::RecentlyPlayed,
        Self::Favourites,
        Self::Playlists,
    ];

    /// The row's label, spelled as the issue names it (S2).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Library => "Local library",
            Self::Radio => "Radio",
            Self::Youtube => "YouTube",
            Self::RecentlyPlayed => "Recently played",
            Self::Favourites => "Favourites",
            Self::Playlists => "Playlists",
        }
    }
}

/// The sources screen: which row the cursor is on. There is no list to hold,
/// because [`Source::ALL`] is the list.
#[derive(Debug, Default)]
pub struct Sources {
    selected: usize,
}

impl Sources {
    /// The row the cursor is on.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// The source the cursor is on, which `enter` opens.
    #[must_use]
    pub const fn current(&self) -> Source {
        Source::ALL[self.selected]
    }

    /// The rows, in order, as the screen draws them (S2).
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        Source::ALL
            .into_iter()
            .map(|source| source.label().to_owned())
            .collect()
    }

    /// Moves down one row, wrapping from the last one to the first (issue
    /// #36).
    pub fn select_next(&mut self) {
        self.selected = (self.selected + 1) % Source::ALL.len();
    }

    /// Moves up one row, wrapping from the first one to the last (issue
    /// #36).
    pub fn select_previous(&mut self) {
        self.selected = (self.selected + Source::ALL.len() - 1) % Source::ALL.len();
    }

    /// Moves to the first row.
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// Moves to the last row.
    pub fn select_last(&mut self) {
        self.selected = Source::ALL.len() - 1;
    }
}
