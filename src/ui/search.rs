//! The fuzzy matcher behind `/` (T7, S9): query text in, ranked candidate
//! indices out. No terminal, no library, no audio lives here — that is what
//! makes it testable, and it is all `tests/search.rs` pins.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// One open search: the query as the user typed it, plus [`Search::rank`],
/// which maps candidate strings to indices into the order they were given.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Search {
    query: String,
}

impl Search {
    /// A search with an empty query, matching everything in order.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one typed character.
    pub fn push(&mut self, character: char) {
        self.query.push(character);
    }

    /// Removes the last character, or `None` when there is nothing to remove.
    pub fn pop(&mut self) -> Option<char> {
        self.query.pop()
    }

    /// The query as typed.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the query is empty — which still "matches": every candidate,
    /// in the order given.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// The indices of the candidates the query matches, best first. An empty
    /// query matches everything and keeps the given order. Ranking prefers an
    /// exact match, then a prefix, then the matcher's own fuzzy score.
    #[must_use]
    pub fn rank(&self, candidates: &[impl AsRef<str>]) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..candidates.len()).collect();
        }
        // Spaces split the query into words, each matched on its own, so one
        // query reaches across artist, album and title.
        let pattern = Pattern::new(
            &self.query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        // Exact and prefix are decided on the folded texts, not on scores.
        let query = self.query.to_lowercase();
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(usize, u8, u32)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let candidate = candidate.as_ref();
                let score = pattern.score(Utf32Str::new(candidate, &mut buf), &mut matcher)?;
                let folded = candidate.to_lowercase();
                let tier = if folded == query {
                    0 // exact
                } else if folded.starts_with(&query) {
                    1 // prefix
                } else {
                    2 // scattered
                };
                Some((index, tier, score))
            })
            .collect();
        scored.sort_by_key(|&(index, tier, score)| (tier, std::cmp::Reverse(score), index));
        scored.into_iter().map(|(index, _, _)| index).collect()
    }
}
