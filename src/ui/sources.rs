//! The first screen (issue #3): the sources velo can play, one row each,
//! with the cursor already on the first one. The list is fixed — two
//! entries that are compiled in — so this module holds the cursor and the
//! labels, nothing else.

/// A source the player can open: the one dimension the library and the radio
/// have in common.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// The local library index, with its four views inside it.
    #[default]
    Library,
    /// Live radio: the directory, the country, the user's own stations.
    Radio,
}

impl Source {
    /// Every source, in the order the first screen shows them (S2).
    pub const ALL: [Self; 2] = [Self::Library, Self::Radio];

    /// The row's label, spelled as the issue names it (S2).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Library => "Local library",
            Self::Radio => "Radio",
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

    /// Moves down one row, and stops at the last one.
    pub fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(Source::ALL.len() - 1);
    }

    /// Moves up one row, and stops at the first one.
    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
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
