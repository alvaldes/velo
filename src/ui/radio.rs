//! The radio view (T6a, S9): one list of sections over the station
//! directory, navigated with the keys the rest of the interface already
//! uses.
//!
//! The view is a pure state machine: it holds the loaded indexes (countries,
//! tags), the user's own country, the preference store, the one catalog slot
//! for the place or tag currently loaded, and the cursor and filter of the
//! focused list. It never touches the network — the caller loads what a
//! section asks for (a country's stations, a tag's stations) and hands it
//! back through [`RadioView::set_catalog`] — and it never plays on its own:
//! `enter` on a station answers [`RadioOutcome::Play`] and `a` answers
//! [`RadioOutcome::Queue`], and the shell — which owns the queue and the
//! player — carries them out (T6c).
//!
//! The sections, in order and only when they have content: the favourites
//! (`★ Name [codec] · CC`) first, browse all countries, the user's own
//! country (`Norway (near you)`) with its regions once their index is
//! fetched (T6d), the pinned places (`★ Germany`), the stations that belong
//! to no country, browse genres & tags, and the loaded catalog
//! (`Name [128k] · CC`), with stations the codec filter rejects marked so
//! the user can tell before pressing enter.
//!
//! Station identity is never raw string equality: marking goes through
//! [`RadioStore`]'s toggles, which decide on [`station_key`] alone.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};

use crate::radio::client::{Country, Order, State, Station, Tag};
use crate::radio::directory::is_playable_station;
use crate::radio::store::{Favorite, Pinned, RadioStore};

use super::plural;
use super::theme::Theme;
use super::{ListRow, centered, digits, draw_empty, list_row};

/// What one key press asked the shell to do with the radio view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadioOutcome {
    /// The key moved radio state; the shell only redraws.
    Handled,
    /// The view closed itself (`R`, or `esc` from the top level).
    Close,
    /// `tab` left the radio for the queue.
    ToQueue,
    /// `enter` on a playable station (T6c): the shell queues the station
    /// and starts it, the way `enter` on a queue row plays from that row.
    /// A station the codec filter rejects is never handed over (S8).
    Play {
        /// The stream URL the directory listed (`url_resolved`).
        url: String,
        /// The station's own, user-facing name.
        name: String,
    },
    /// `a` on a playable station (T6c): the shell appends the station to
    /// the queue without starting it, and the view stays open.
    Queue {
        /// The stream URL the directory listed (`url_resolved`).
        url: String,
        /// The station's own, user-facing name.
        name: String,
    },
    /// Not a radio key: the rest of the keymap may have it.
    Pass,
}

/// Where in the radio the view stands. Every level below [`Place::Top`] was
/// reached by `enter` and is left by `esc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// The section list.
    Top,
    /// The country index (`Browse all countries`, or `N`).
    Countries,
    /// The tag index (`Browse genres & tags`).
    Tags,
    /// The stations that belong to no country: the ones the user added by
    /// hand, and favourites saved without a `countrycode`. The user's own
    /// data, so it is never a directory request.
    NoCountry,
    /// The stations of one country, or of one of its regions.
    Country {
        /// ISO 3166-1 code.
        code: String,
        /// Display name.
        name: String,
        /// Region within the country; empty for the country itself.
        state: String,
    },
    /// The stations of one tag.
    Tag {
        /// The tag's name.
        name: String,
    },
}

/// What one visible row is. Station rows carry enough to mark and toggle
/// them through the store; place rows carry where `enter` goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// `Browse all countries`.
    BrowseCountries,
    /// `Browse genres & tags`.
    BrowseTags,
    /// `Stations without a country`: the place holding the stations that
    /// belong to no country (S4). It opens [`Place::NoCountry`].
    BrowseNoCountry,
    /// `Use my location`: the country question, shown while the store has
    /// no answer. `y` on the row stores the detected country, `n` stores
    /// the decline; no modal of its own (T6b, S5).
    AskCountry {
        /// The ISO 3166-1 code the locale yields, stored on `y`.
        code: String,
    },
    /// The user's own country.
    NearYou {
        /// ISO 3166-1 code.
        code: String,
    },
    /// A pinned country or region.
    Pinned {
        /// ISO 3166-1 code.
        code: String,
        /// Display name.
        name: String,
        /// Region within the country; empty for the country itself.
        state: String,
    },
    /// One country of the index.
    Country {
        /// ISO 3166-1 code.
        code: String,
        /// Display name.
        name: String,
    },
    /// One region of the user's own country, from `/states/<home>/`
    /// (T6d, S5). It opens the region's stations like a country does and
    /// pins through [`Pinned`] with the region in `state`.
    Region {
        /// ISO 3166-1 code of the country the region is in.
        code: String,
        /// The country's display name.
        name: String,
        /// The region's name.
        state: String,
    },
    /// One tag of the index.
    Tag {
        /// The tag's name.
        name: String,
    },
    /// One station of the loaded catalog. `playable` is the codec filter's
    /// answer: a station it rejects is marked on screen and never queues.
    CatalogStation {
        /// The station as the directory listed it.
        station: Station,
        /// Whether the codec filter would let it play.
        playable: bool,
    },
    /// One hand-added station.
    AddedStation {
        /// The name the user gave it.
        name: String,
        /// The stream URL; the identity is [`station_key`] of this.
        url: String,
    },
    /// One favourited station, as the store keeps it.
    FavoriteStation {
        /// The stored favourite.
        favorite: Favorite,
    },
}

/// One visible row: what it says, the dim fact against the right edge and
/// what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The row's visible label — what the filter matches against.
    pub label: String,
    /// The dim note against the right edge, when there is one.
    pub fact: String,
    /// What the row is.
    pub kind: RowKind,
}

/// The order overlay's rows, in display order: the label and the
/// [`Order`] it applies.
const ORDER_ROWS: [(&str, Order); 5] = [
    ("Most Voted", Order::Votes),
    ("Most Listened", Order::ClickCount),
    ("Trending", Order::ClickTrend),
    ("By Name", Order::Name),
    ("Random", Order::Random),
];

/// The radio view. Build it with [`RadioView::new`] over the loaded indexes
/// and the preference store, or [`RadioView::empty`] when nothing is loaded
/// yet; the sections appear only when they have content.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flags are independent facts of the view: whether the filter\n    is being typed, whether the wired view wants its browse rows before\n    their indexes load, whether the store changed since the last save ask,\n    and whether ctrl-r asked for a refresh; bundling them would obscure each"
)]
#[derive(Debug)]
pub struct RadioView {
    /// The station preferences: favourites, pins, hand-added stations and
    /// the opt-in country. The view owns it and marks through it, so the
    /// caller reads the changes back with [`RadioView::store`] and saves.
    store: RadioStore,
    /// The country index, as `/countries` answered.
    countries: Vec<Country>,
    /// The user's own country's regions, as `/states/<home>/` answered
    /// (T6d, S5).
    states: Vec<State>,
    /// The tag index, as `/tags` answered.
    tags: Vec<Tag>,
    /// The user's own ISO 3166-1 code, when `country::detect` and the
    /// store's `country` agree that there is one (S5). The caller resolves
    /// that once and hands the answer in.
    own_country: Option<String>,
    /// The stations of the place or tag currently loaded — the one catalog
    /// slot. Replaced whole by [`RadioView::set_catalog`], never merged.
    catalog: Vec<Station>,
    /// Where the view stands.
    place: Place,
    /// The places `enter` went through, most recent first: what `esc`
    /// returns to.
    stack: Vec<Place>,
    /// The selected row of the visible list.
    cursor: usize,
    /// The filter text typed after `/`.
    filter: String,
    /// Whether printable keys type into the filter (between `/` and `enter`).
    typing: bool,
    /// The order a place's stations are loaded in.
    order: Order,
    /// The order overlay's cursor while it is open; `None` when it is not.
    order_card: Option<usize>,
    /// Whether the two browse rows show before their indexes load: the
    /// wired view wants them from the start (T6b); the bare constructors
    /// keep the sections appearing only with content.
    browse_rows: bool,
    /// The country the locale yields, while the store has no answer: the
    /// `Use my location` row stands on it (S5, T6b).
    country_ask: Option<String>,
    /// Whether the store changed since the last save ask: the wiring reads
    /// this and persists the file.
    dirty: bool,
    /// Whether `ctrl-r` asked the caller for a fresh directory, not yet
    /// taken.
    refresh_wanted: bool,
}

impl RadioView {
    /// A radio view over the given indexes, store and own country. The
    /// indexes may be empty — the sections simply do not appear.
    #[must_use]
    pub fn new(
        store: RadioStore,
        countries: Vec<Country>,
        tags: Vec<Tag>,
        own_country: Option<String>,
    ) -> Self {
        Self {
            store,
            countries,
            states: Vec::new(),
            tags,
            own_country,
            catalog: Vec::new(),
            place: Place::Top,
            stack: Vec::new(),
            cursor: 0,
            filter: String::new(),
            typing: false,
            order: Order::default(),
            order_card: None,
            browse_rows: false,
            country_ask: None,
            dirty: false,
            refresh_wanted: false,
        }
    }

    /// A radio view with nothing loaded: no sections, an empty index, no
    /// "near you" row. The shell opens this on `R` until the directory and
    /// the store are wired in.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(RadioStore::default(), Vec::new(), Vec::new(), None)
    }

    /// Replaces the loaded catalog with the stations of the current place or
    /// tag, in the order the caller loaded them. The catalog is one slot:
    /// loading a new place replaces the old list whole.
    pub fn set_catalog(&mut self, stations: Vec<Station>) {
        self.catalog = stations;
        self.clamp();
    }

    /// Replaces the country index, as `/countries` answered (T6b). An
    /// empty answer is still an answer: the section stays, its count says
    /// zero.
    pub fn set_countries(&mut self, countries: Vec<Country>) {
        self.countries = countries;
        self.clamp();
    }

    /// Replaces the tag index, as `/tags` answered (T6b).
    pub fn set_tags(&mut self, tags: Vec<Tag>) {
        self.tags = tags;
        self.clamp();
    }

    /// Replaces the home country's state index, as `/states/<home>/`
    /// answered (T6d). The rows appear at the top beside the places; an
    /// empty answer is still an answer.
    pub fn set_states(&mut self, states: Vec<State>) {
        self.states = states;
        self.clamp();
    }

    /// Shows the two browse rows before their indexes load, so the
    /// sections can be entered and the loading can start lazily (T6b).
    pub fn show_browse_rows(&mut self) {
        self.browse_rows = true;
    }

    /// Sets the country the locale yields while the store has no answer
    /// (S5, T6b). `Some` shows the one `Use my location` row, answered
    /// with `y`/`n` right on it; `None` — the locale yields no country —
    /// shows no row and nothing else is gated on it.
    pub fn ask_country(&mut self, detected: Option<String>) {
        self.country_ask = detected;
    }

    /// Whether the store changed since the last save ask, taking the ask:
    /// the wiring saves the file when this answers true (T6b).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Where the view stands.
    #[must_use]
    pub fn place(&self) -> &Place {
        &self.place
    }

    /// The order a place's stations are loaded in, as the order overlay set
    /// it last.
    #[must_use]
    pub const fn order(&self) -> Order {
        self.order
    }

    /// The preference store the view marks through.
    #[must_use]
    pub const fn store(&self) -> &RadioStore {
        &self.store
    }

    /// The selected row of the visible list, counted from zero.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the order overlay is open.
    #[must_use]
    pub const fn order_open(&self) -> bool {
        self.order_card.is_some()
    }

    /// The order overlay's focused row, counted from zero.
    #[must_use]
    pub fn order_cursor(&self) -> usize {
        self.order_card.unwrap_or(0)
    }

    /// Whether a filter is being typed or is applied to the focused list.
    #[must_use]
    pub const fn filtering(&self) -> bool {
        self.typing || !self.filter.is_empty()
    }

    /// Whether printable keys are typing into the filter right now.
    #[must_use]
    pub const fn typing(&self) -> bool {
        self.typing
    }

    /// Whether `ctrl-r` asked for a fresh directory, taking the ask: a
    /// second call answers `false` until `ctrl-r` is pressed again.
    pub fn take_refresh_request(&mut self) -> bool {
        std::mem::take(&mut self.refresh_wanted)
    }

    /// The visible rows of the focused list, filter applied, in order.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<Row> {
        let needle = self.filter.to_lowercase();
        self.rows()
            .into_iter()
            .filter(|row| needle.is_empty() || fuzzy(&needle, &row.label.to_lowercase()))
            .collect()
    }

    /// The labels of the visible rows, in order.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.visible_rows()
            .into_iter()
            .map(|row| row.label)
            .collect()
    }

    /// The row the cursor stands on, if the list has one.
    #[must_use]
    pub fn selection(&self) -> Option<Row> {
        self.visible_rows().into_iter().nth(self.cursor)
    }

    /// Applies one key press. Keys the view does not own answer
    /// [`RadioOutcome::Pass`] and still reach the rest of the keymap, so
    /// `space`, the volume and `q` keep working over the radio.
    #[must_use]
    pub fn key(&mut self, event: KeyEvent) -> RadioOutcome {
        if self.order_card.is_some() {
            self.order_key(event.code);
            return RadioOutcome::Handled;
        }
        if self.typing {
            self.typing_key(event.code);
            return RadioOutcome::Handled;
        }
        match event.code {
            KeyCode::Char('R') => RadioOutcome::Close,
            KeyCode::Esc | KeyCode::Backspace => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.clamp();
                    RadioOutcome::Handled
                } else if self.back() {
                    RadioOutcome::Handled
                } else {
                    RadioOutcome::Close
                }
            }
            KeyCode::Tab => RadioOutcome::ToQueue,
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                RadioOutcome::Handled
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                RadioOutcome::Handled
            }
            KeyCode::Char('g') => {
                self.select_first();
                RadioOutcome::Handled
            }
            KeyCode::Char('G') => {
                self.select_last();
                RadioOutcome::Handled
            }
            KeyCode::Enter => self.enter(),
            KeyCode::Char('a') => self.append_station(),
            KeyCode::Char('/') => {
                self.typing = true;
                RadioOutcome::Handled
            }
            KeyCode::Char('f') => {
                self.toggle_mark();
                RadioOutcome::Handled
            }
            KeyCode::Char('N') => {
                self.open_countries();
                RadioOutcome::Handled
            }
            KeyCode::Char('y' | 'n') if self.on_ask_row() => {
                self.answer_country(event.code == KeyCode::Char('y'));
                RadioOutcome::Handled
            }
            KeyCode::Char('o') => {
                self.open_order();
                RadioOutcome::Handled
            }
            KeyCode::Char('r') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.refresh_wanted = true;
                RadioOutcome::Handled
            }
            _ => RadioOutcome::Pass,
        }
    }

    /// The keys the open order overlay owns: it answers everything, the way
    /// the settings card does.
    fn order_key(&mut self, code: KeyCode) {
        let Some(cursor) = self.order_card.as_mut() else {
            return;
        };
        let last = ORDER_ROWS.len() - 1;
        match code {
            KeyCode::Char('j') | KeyCode::Down => *cursor = (*cursor + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => *cursor = cursor.saturating_sub(1),
            KeyCode::Char('g') => *cursor = 0,
            KeyCode::Char('G') => *cursor = last,
            KeyCode::Enter => {
                self.order = ORDER_ROWS[*cursor].1;
                self.order_card = None;
            }
            KeyCode::Esc => self.order_card = None,
            _ => {}
        }
    }

    /// The keys the typing filter owns: characters and `backspace` edit,
    /// `enter` keeps the filter, `esc` clears it, and `j`/`k` still move.
    fn typing_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.clamp();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.clamp();
            }
            KeyCode::Enter => self.typing = false,
            KeyCode::Esc => {
                self.typing = false;
                self.filter.clear();
                self.clamp();
            }
            _ => {}
        }
    }

    /// Moves the cursor down one visible row, clamped at the end.
    pub fn select_next(&mut self) {
        let last = self.visible_rows().len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(last);
    }

    /// Moves the cursor up one visible row, clamped at the start.
    pub fn select_prev(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor to the first visible row.
    pub fn select_first(&mut self) {
        self.cursor = 0;
    }

    /// Moves the cursor to the last visible row.
    pub fn select_last(&mut self) {
        self.cursor = self.visible_rows().len().saturating_sub(1);
    }

    /// Keeps the cursor inside the visible list after its length changed.
    fn clamp(&mut self) {
        let last = self.visible_rows().len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
    }

    /// Opens the highlighted section or row: a browse row opens its index, a
    /// place row enters the place, and a station row hands the station to
    /// the shell to queue and play ([`RadioOutcome::Play`], T6c) — a station
    /// the codec filter rejects is never handed over (S8). Nothing here
    /// plays; the shell does.
    #[must_use]
    pub fn enter(&mut self) -> RadioOutcome {
        let Some(row) = self.selection() else {
            return RadioOutcome::Handled;
        };
        if let Some((url, name)) = station_of_row(&row.kind) {
            return RadioOutcome::Play { url, name };
        }
        let next = match row.kind {
            RowKind::BrowseCountries => Some(Place::Countries),
            RowKind::BrowseTags => Some(Place::Tags),
            RowKind::BrowseNoCountry => Some(Place::NoCountry),
            RowKind::NearYou { code } | RowKind::Country { code, .. } => {
                Some(self.country_place(&code, ""))
            }
            RowKind::Region { code, name, state } => Some(Place::Country { code, name, state }),
            RowKind::Pinned {
                code,
                name: _,
                state,
            } => Some(self.country_place(&code, &state)),
            RowKind::Tag { name } => Some(Place::Tag { name }),
            RowKind::CatalogStation { .. }
            | RowKind::AddedStation { .. }
            | RowKind::FavoriteStation { .. }
            | RowKind::AskCountry { .. } => None,
        };
        if let Some(place) = next {
            self.stack.push(self.place.clone());
            self.place = place;
            self.cursor = 0;
            self.filter.clear();
            self.typing = false;
        }
        RadioOutcome::Handled
    }

    /// `a` on the selected row: a playable station hands itself to the
    /// shell to queue without starting and without leaving the view
    /// ([`RadioOutcome::Queue`], T6c); any other row is consumed and
    /// queues nothing.
    fn append_station(&mut self) -> RadioOutcome {
        let Some(row) = self.selection() else {
            return RadioOutcome::Handled;
        };
        station_of_row(&row.kind).map_or(RadioOutcome::Handled, |(url, name)| RadioOutcome::Queue {
            url,
            name,
        })
    }

    /// The place of one country, named from the index when it is there.
    fn country_place(&self, code: &str, state: &str) -> Place {
        let name = self
            .countries
            .iter()
            .find(|country| country.iso_3166_1 == code)
            .map_or_else(|| code.to_owned(), |country| country.name.clone());
        Place::Country {
            code: code.to_owned(),
            name,
            state: state.to_owned(),
        }
    }

    /// Goes back one level, restoring where `enter` came from. At the top
    /// there is nothing to go back to: the answer is `false` and the shell
    /// closes the radio.
    pub fn back(&mut self) -> bool {
        match self.stack.pop() {
            Some(place) => {
                self.place = place;
                self.cursor = 0;
                self.filter.clear();
                self.typing = false;
                true
            }
            None => false,
        }
    }

    /// Opens the country index from anywhere.
    pub fn open_countries(&mut self) {
        if self.place != Place::Countries {
            self.stack.push(self.place.clone());
            self.place = Place::Countries;
            self.cursor = 0;
            self.filter.clear();
            self.typing = false;
        }
    }

    /// Opens the order overlay, cursor on the current order.
    pub fn open_order(&mut self) {
        let cursor = ORDER_ROWS
            .iter()
            .position(|(_, order)| *order == self.order)
            .unwrap_or(0);
        self.order_card = Some(cursor);
    }

    /// Answers the country question on the selected `Use my location` row:
    /// `y` stores the locale's country, `n` stores the decline. Either
    /// answer marks the store for saving; the row then becomes the
    /// near-you row, or goes away (T6b, S5).
    fn answer_country(&mut self, yes: bool) {
        let Some(code) = self.country_ask.take() else {
            return;
        };
        if yes {
            self.own_country = Some(code.clone());
            self.store.country = Some(code);
        } else {
            self.store.country = Some(String::from("none"));
        }
        self.dirty = true;
    }

    /// Whether the cursor stands on the `Use my location` row.
    fn on_ask_row(&self) -> bool {
        self.selection()
            .is_some_and(|row| matches!(row.kind, RowKind::AskCountry { .. }))
    }

    /// `f` on the selected row: a station toggles its favourite, a place
    /// toggles its pin, a browse row and a tag do nothing. Both toggles
    /// decide on the station key, never on the URL as written. Every
    /// change marks the store for saving (T6b).
    pub fn toggle_mark(&mut self) {
        let Some(row) = self.selection() else {
            return;
        };
        match row.kind {
            RowKind::CatalogStation { station, .. } => {
                let toggled = self.store.toggle_favorite(Favorite {
                    name: station.name,
                    url: station.url_resolved,
                    countrycode: station.countrycode,
                    codec: station.codec,
                });
                self.dirty |= toggled.needs_save();
            }
            RowKind::AddedStation { name, url } => {
                let toggled = self.store.toggle_favorite(Favorite {
                    name,
                    url,
                    countrycode: String::new(),
                    codec: String::new(),
                });
                self.dirty |= toggled.needs_save();
            }
            RowKind::FavoriteStation { favorite } => {
                let toggled = self.store.toggle_favorite(favorite);
                self.dirty |= toggled.needs_save();
            }
            RowKind::NearYou { code } | RowKind::Country { code, .. } => {
                let name = self.country_name(&code);
                let toggled = self.store.toggle_pin(Pinned {
                    code,
                    name,
                    state: String::new(),
                });
                self.dirty |= toggled.needs_save();
            }
            RowKind::Pinned { code, name, state } | RowKind::Region { code, name, state } => {
                let toggled = self.store.toggle_pin(Pinned { code, name, state });
                self.dirty |= toggled.needs_save();
            }
            RowKind::BrowseCountries
            | RowKind::BrowseTags
            | RowKind::BrowseNoCountry
            | RowKind::Tag { .. }
            | RowKind::AskCountry { .. } => {}
        }
    }

    /// The display name of one ISO code, from the index when it is there.
    fn country_name(&self, code: &str) -> String {
        self.countries
            .iter()
            .find(|country| country.iso_3166_1 == code)
            .map_or_else(|| code.to_owned(), |country| country.name.clone())
    }

    /// Every row of the focused list, unfiltered.
    fn rows(&self) -> Vec<Row> {
        match &self.place {
            Place::Top => self.top_rows(),
            Place::Countries => self
                .countries
                .iter()
                .map(|country| Row {
                    label: country.name.clone(),
                    fact: plural(
                        usize::try_from(country.stationcount).unwrap_or(usize::MAX),
                        "station",
                        "stations",
                    ),
                    kind: RowKind::Country {
                        code: country.iso_3166_1.clone(),
                        name: country.name.clone(),
                    },
                })
                .collect(),
            Place::Tags => self
                .tags
                .iter()
                .map(|tag| Row {
                    label: tag.name.clone(),
                    fact: plural(
                        usize::try_from(tag.stationcount).unwrap_or(usize::MAX),
                        "station",
                        "stations",
                    ),
                    kind: RowKind::Tag {
                        name: tag.name.clone(),
                    },
                })
                .collect(),
            Place::Country { .. } | Place::Tag { .. } => self.catalog_rows(),
            Place::NoCountry => self.no_country_rows(),
        }
    }

    /// The stations that belong to no country (S4): every hand-added
    /// station first, then the favourites saved without a `countrycode`.
    /// The same [`RowKind`]s as everywhere else, so playing, queueing and
    /// marking keep working unchanged.
    fn no_country_rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .store
            .station
            .iter()
            .map(|added| Row {
                label: added.name.clone(),
                fact: String::new(),
                kind: RowKind::AddedStation {
                    name: added.name.clone(),
                    url: added.url.clone(),
                },
            })
            .collect();
        rows.extend(
            self.store
                .favorite
                .iter()
                .filter(|favorite| favorite.countrycode.is_empty())
                .map(|favorite| Row {
                    label: favorite_label(favorite),
                    fact: String::new(),
                    kind: RowKind::FavoriteStation {
                        favorite: favorite.clone(),
                    },
                }),
        );
        rows
    }

    /// The top level: the section rows and the station lists that show
    /// directly, in the order the layout fixed.
    fn top_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        // The favourites first (S5): the stations someone actually listens
        // to are one keypress away instead of buried under a country. A
        // favourite with no country also shows in its section below.
        for favorite in &self.store.favorite {
            rows.push(Row {
                label: favorite_label(favorite),
                fact: String::new(),
                kind: RowKind::FavoriteStation {
                    favorite: favorite.clone(),
                },
            });
        }
        // The browse rows belong to the sections, not to their content:
        // the wired view shows them before the indexes load, so entering
        // one can start the load (T6b).
        if self.browse_rows || !self.countries.is_empty() {
            let fact = if self.countries.is_empty() {
                String::new()
            } else {
                plural(self.countries.len(), "country", "countries")
            };
            rows.push(Row {
                label: "Browse all countries".to_owned(),
                fact,
                kind: RowKind::BrowseCountries,
            });
        }
        // The country question takes the near-you row's place while it is
        // unanswered: one row, answered with `y`/`n` right on it (T6b, S5).
        if self.store.country.is_none()
            && let Some(code) = self.country_ask.clone()
        {
            rows.push(Row {
                label: "Use my location".to_owned(),
                fact: String::new(),
                kind: RowKind::AskCountry { code },
            });
        }
        if let Some(code) = self.own_country.clone() {
            let name = self.country_name(&code);
            rows.push(Row {
                label: format!("{name} (near you)"),
                fact: String::new(),
                kind: RowKind::NearYou { code },
            });
        }
        // The home country's regions, once `/states/<home>/` answered
        // (T6d, S5): rows beside the places, each opening its region's
        // stations and pinnable like a country. They sit right under the
        // near-you row they belong to.
        if self.own_country.is_some() {
            for state in &self.states {
                rows.push(Row {
                    label: format!("{} › {}", state.country, state.name),
                    fact: plural(
                        usize::try_from(state.stationcount).unwrap_or(usize::MAX),
                        "station",
                        "stations",
                    ),
                    kind: RowKind::Region {
                        code: self.own_country.clone().expect("just checked"),
                        name: state.country.clone(),
                        state: state.name.clone(),
                    },
                });
            }
        }
        for pinned in &self.store.pinned {
            let label = if pinned.state.is_empty() {
                format!("★ {}", pinned.name)
            } else {
                format!("★ {} · {}", pinned.name, pinned.state)
            };
            rows.push(Row {
                label,
                fact: String::new(),
                kind: RowKind::Pinned {
                    code: pinned.code.clone(),
                    name: pinned.name.clone(),
                    state: pinned.state.clone(),
                },
            });
        }
        // The stations that belong to no country (S4): the user's own
        // hand-added stations and the favourites saved without a
        // `countrycode`, one `enter` away and never a directory request.
        let without_country = self.store.station.len()
            + self
                .store
                .favorite
                .iter()
                .filter(|favorite| favorite.countrycode.is_empty())
                .count();
        if without_country > 0 {
            rows.push(Row {
                label: "Stations without a country".to_owned(),
                fact: plural(without_country, "station", "stations"),
                kind: RowKind::BrowseNoCountry,
            });
        }
        if self.browse_rows || !self.tags.is_empty() {
            let fact = if self.tags.is_empty() {
                String::new()
            } else {
                plural(self.tags.len(), "tag", "tags")
            };
            rows.push(Row {
                label: "Browse genres & tags".to_owned(),
                fact,
                kind: RowKind::BrowseTags,
            });
        }
        rows.extend(self.catalog_rows());
        rows
    }

    /// The loaded catalog: every station, favourites starred, stations the
    /// directory rejects as unplayable marked with `✗`.
    fn catalog_rows(&self) -> Vec<Row> {
        self.catalog
            .iter()
            .map(|station| {
                let playable = is_playable_station(station);
                let favorite = self.store.is_favorite(&station.url_resolved);
                let mut label = String::new();
                if !playable {
                    label.push_str("✗ ");
                }
                if favorite {
                    label.push('★');
                }
                label.push_str(&station.name);
                label.push_str(&station_facts(station));
                Row {
                    label,
                    fact: String::new(),
                    kind: RowKind::CatalogStation {
                        station: station.clone(),
                        playable,
                    },
                }
            })
            .collect()
    }

    /// What the header line says where the view stands and how many rows it
    /// lists.
    fn header(&self) -> (String, String) {
        let (label, one, many) = match &self.place {
            Place::Top => ("Radio".to_owned(), "section", "sections"),
            Place::Countries => ("Radio › countries".to_owned(), "country", "countries"),
            Place::Tags => ("Radio › tags".to_owned(), "tag", "tags"),
            Place::Country { name, state, .. } => {
                if state.is_empty() {
                    (format!("Radio › {name}"), "station", "stations")
                } else {
                    (format!("Radio › {name} › {state}"), "station", "stations")
                }
            }
            Place::Tag { name } => (format!("Radio › {name}"), "station", "stations"),
            Place::NoCountry => (
                "Radio › stations without a country".to_owned(),
                "station",
                "stations",
            ),
        };
        (label, plural(self.visible_rows().len(), one, many))
    }

    /// Draws the view into `area`: the header line, the list with the
    /// standard list styling, and the order overlay centred over it while it
    /// is open. Nothing outside `area` is touched.
    pub fn render(&self, frame: &mut ratatui::Frame, area: Rect, theme: &Theme) {
        frame.render_widget(Block::new().style(Style::new().bg(theme.background)), area);
        let [header_area, list_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

        let (label, count) = self.header();
        let filter_text = if self.filtering() {
            format!("  /{}", self.filter)
        } else {
            String::new()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▸─ ", Style::new().fg(theme.border)),
                Span::styled(label, Style::new().fg(theme.text).bold()),
                Span::styled(
                    format!("{filter_text}  [{count}]"),
                    Style::new().fg(theme.dim),
                ),
            ])),
            header_area,
        );

        let visible = self.visible_rows();
        if visible.is_empty() {
            let message = if self.filtering() {
                "no station matches"
            } else {
                "no stations here"
            };
            draw_empty(frame, list_area, message, theme.dim);
        } else {
            let rows: Vec<ListItem<'static>> = visible
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    ListItem::new(list_row(
                        ListRow {
                            playing: false,
                            number: index + 1,
                            digits: digits(visible.len()),
                            name: row.label.clone(),
                            note: String::new(),
                            fact: row.fact.clone(),
                        },
                        list_area.width,
                        theme,
                    ))
                })
                .collect();
            let list = List::new(rows)
                .style(Style::new().bg(theme.background))
                .highlight_style(Style::new().bg(theme.accent).fg(theme.ink));
            let mut state = ListState::default().with_selected(Some(self.cursor));
            frame.render_stateful_widget(list, list_area, &mut state);
        }

        if self.order_card.is_some() {
            self.draw_order_card(frame, area, theme);
        }
    }

    /// The order overlay: a small rounded card, centred like the settings
    /// card, with the five orders and the focused one in the accent.
    fn draw_order_card(&self, frame: &mut ratatui::Frame, area: Rect, theme: &Theme) {
        let width = u16::try_from(
            ORDER_ROWS
                .iter()
                .map(|(label, _)| label.chars().count())
                .max()
                .unwrap_or(0)
                + 4,
        )
        .unwrap_or(area.width);
        let height = u16::try_from(ORDER_ROWS.len() + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);
        let cursor = self.order_card.unwrap_or(0);
        let lines = ORDER_ROWS
            .iter()
            .enumerate()
            .map(|(index, (label, _))| {
                let style = if index == cursor {
                    Style::new().fg(theme.accent)
                } else {
                    Style::new().fg(theme.text)
                };
                Line::styled((*label).to_owned(), style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(
                        " order ",
                        Style::new().fg(theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::horizontal(1))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }
}

/// The stream URL and name of a row when it is a station the codec filter
/// lets play (T6c). A station the filter rejects never queues (S8): it is
/// marked on screen instead. Hand-added stations and favourites were put
/// there deliberately, so no filter stands between them and the queue.
fn station_of_row(kind: &RowKind) -> Option<(String, String)> {
    match kind {
        RowKind::CatalogStation {
            station,
            playable: true,
        } => Some((station.url_resolved.clone(), station.name.clone())),
        RowKind::AddedStation { name, url } => Some((url.clone(), name.clone())),
        RowKind::FavoriteStation { favorite } => {
            Some((favorite.url.clone(), favorite.name.clone()))
        }
        _ => None,
    }
}

/// `★ Name [MP3] · NO`, leaving out what the store does not know. The store
/// keeps no bitrate, so a favourite shows its codec where a catalog station
/// shows `128k`.
fn favorite_label(favorite: &Favorite) -> String {
    use std::fmt::Write as _;

    let mut label = format!("★ {}", favorite.name);
    if !favorite.codec.is_empty() {
        let _ = write!(label, " [{}]", favorite.codec);
    }
    if !favorite.countrycode.is_empty() {
        let _ = write!(label, " · {}", favorite.countrycode);
    }
    label
}

/// ` [128k] · NO` for a catalog station, each part only when known.
fn station_facts(station: &Station) -> String {
    use std::fmt::Write as _;

    let mut facts = String::new();
    if station.bitrate > 0 {
        let _ = write!(facts, " [{}k]", station.bitrate);
    }
    if !station.countrycode.is_empty() {
        let _ = write!(facts, " · {}", station.countrycode);
    }
    facts
}

/// Fuzzy-ish matching: every character of the needle appears in the
/// haystack, in order — `ger` finds `★ Germany`, `nv` finds `Nova`.
fn fuzzy(needle: &str, haystack: &str) -> bool {
    let mut characters = haystack.chars();
    needle
        .chars()
        .all(|wanted| characters.any(|seen| seen == wanted))
}
