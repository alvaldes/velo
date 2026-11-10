//! The radio view's data wiring (T6b, S9): the preference store, the
//! opt-in country, and the directory requests — on a worker thread, so the
//! interface never waits on the network.
//!
//! This follows the library index's pattern ([`crate::ui::indexing`]): a
//! request runs on its own thread and hands its outcome back through a
//! channel that the interface polls on every turn of its loop. One request
//! is in flight at a time; while it runs the shell shows `⟳ loading…` and
//! keeps answering keys.
//!
//! Loading is lazy, not eager: opening the view makes zero requests. The
//! country index loads when `Browse all countries` is entered, the tag
//! index when `Browse genres & tags` is entered, a place's stations when
//! the place is opened — capped at [`crate::radio::directory::STATION_CAP`]
//! through [`loadable_stations`] — and `Ctrl+R` re-requests only what the
//! place on screen needs. The stations that belong to no country (S4) are
//! the user's own data, so that place never requests anything. The home
//! country's state index
//! (`/states/<home>/`, T6d) loads beside the country index, for the home
//! country only and never for another; with no home country — the question
//! unanswered or declined — no states request is made at all. An index
//! already loaded is reused; a failed request becomes one status-line
//! message, never a retry behind the user's back.
//!
//! The preference store is the shell's responsibility here: the view marks
//! through it, and a poll that sees a change saves it to `radio.toml`
//! atomically. A store that could not be read is reported once and then
//! never written — preferences that were never seen are never overwritten.
//! The country question is answered on the `Use my location` row itself:
//! `y` stores the locale's country, `n` stores `none`, both save at once.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::radio::cliamp::{self, Channel, Listeners, Song};
use crate::radio::client::{
    self, Client, Country, HttpTransport, Order, State, Station, StationSearch, Tag, Transport,
};
use crate::radio::directory::loadable_stations;
use crate::radio::store::RadioStore;
use crate::ui::radio::{Place, RadioView};

/// The seam between the shell and the radio data. `App` holds one as a
/// trait object so the interface stays free of generics; the tests drive a
/// [`RadioData`] over a canned transport through the same methods.
pub trait RadioWire {
    /// Builds the radio view from what is loaded so far: the store, the
    /// country answer, and every index already fetched this session.
    fn view(&self) -> RadioView;

    /// Runs one turn of the wiring: collects a finished request, saves a
    /// store mutation, and starts whatever the view's current place still
    /// needs. Never blocks.
    fn poll(&mut self, view: &mut RadioView);

    /// The message for the status line, taken once: why a request or a
    /// save failed.
    fn take_message(&mut self) -> Option<String>;

    /// Whether a directory request is in flight right now.
    fn loading(&self) -> bool;
}

/// Hands one transport to both clients: the directory and the cliamp
/// service (issue #48) fetch through the same seam, so a canned transport
/// answers for both and one call log records both.
struct SharedTransport<T>(Arc<T>);

// Manual impls on purpose: the `Arc` carries the sharing, so neither impl
// needs a bound on `T` — a derive would demand `T: Clone`/`T: Debug` for
// nothing.
impl<T> Clone for SharedTransport<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::fmt::Debug for SharedTransport<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SharedTransport")
    }
}

impl<T> SharedTransport<T> {
    /// Wraps one owned transport in the shared arc.
    fn from_transport(transport: T) -> Self {
        Self(Arc::new(transport))
    }
}

impl<T: Transport> Transport for SharedTransport<T> {
    fn get(&self, url: &str) -> Result<String, client::Error> {
        self.0.get(url)
    }
}

/// What one finished background request hands back.
enum Outcome {
    /// The `/countries` index.
    Countries(Result<Vec<Country>, client::Error>),
    /// The `/states/<home>/` index of the user's own country (T6d).
    States(Result<Vec<State>, client::Error>),
    /// The `/tags` index.
    Tags(Result<Vec<Tag>, client::Error>),
    /// The stations of one place, capped by [`loadable_stations`]. The
    /// answer carries the whole [`Place`] it was asked for (S9): it is
    /// cached under that place and renders only in exactly that place.
    Stations {
        /// The place the search was for, so a stale answer is dropped.
        place: Place,
        /// The search's answer.
        result: Result<Vec<Station>, client::Error>,
    },
    /// The cliamp channels: `GET /stations` (issue #48, S2).
    CliampStations(Result<Vec<Channel>, client::Error>),
    /// The cliamp listeners: `GET /statistics` (issue #48, S2).
    CliampStatistics(Result<Vec<Listeners>, client::Error>),
    /// One cliamp channel's songs: `GET /<id>/tracks` (issue #48, S2).
    /// The answer carries the whole [`Place`] it was asked for, so a stale
    /// answer is dropped, the way a station search's is.
    CliampSongs {
        /// The channel place the songs were asked for.
        place: Place,
        /// The answer.
        result: Result<Vec<Song>, client::Error>,
    },
}

/// The search a place's stations load with: the country code, the region
/// when the place names one, or the exact tag, in the view's order, at the
/// directory's own 200-per-place cap. Only a place with a directory
/// request reaches this; [`requests_stations`] gates the rest out.
fn station_search(place: &Place, order: Order) -> StationSearch {
    match place {
        Place::Country { code, state, .. } => StationSearch {
            countrycode: Some(code.clone()),
            state: (!state.is_empty()).then(|| state.clone()),
            order,
            ..StationSearch::default()
        },
        Place::Tag { name } => StationSearch {
            tag: Some(name.clone()),
            order,
            ..StationSearch::default()
        },
        // A name search is not bound to a country, a region or a tag: the
        // directory matches the name alone, so a station is found without
        // guessing where it is filed (S9, issue #15).
        Place::Search { name } => StationSearch {
            name: Some(name.clone()),
            order,
            ..StationSearch::default()
        },
        // The cliamp places (issue #48) are not directory searches: the
        // section lists the service's channels and a channel's songs, and
        // [`requests_stations`] keeps both out of here.
        Place::Top
        | Place::Countries
        | Place::Tags
        | Place::NoCountry
        | Place::Cliamp
        | Place::CliampChannel { .. } => {
            unreachable!("a place without a directory request never searches")
        }
    }
}

/// Whether the place asks the directory for its stations. The no-country
/// place is the user's own data (S4): never a directory request, so an
/// empty section must not produce one — and the top and the two indexes
/// are not station places at all.
fn requests_stations(place: &Place) -> bool {
    matches!(
        place,
        Place::Country { .. } | Place::Tag { .. } | Place::Search { .. }
    )
}

/// The radio data: the client, the store and its file, and the one request
/// channel in flight. Build with [`RadioData::open`] for the shell or
/// [`RadioData::new`] with an injected transport for the tests; hand it to
/// the shell as a [`RadioWire`].
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flags are independent facts: whether the store file was\n    readable, and whether each of the three indexes was already requested\n    this session; each one gates a different request or save"
)]
pub struct RadioData<T> {
    /// The directory client; `None` when the transport could not be built,
    /// in which case every request is skipped and the view still works.
    client: Option<Arc<Client<SharedTransport<T>>>>,
    /// The cliamp client (issue #48), over the same transport; `None`
    /// with it, so a transport that never was leaves the section present
    /// but silent.
    cliamp: Option<Arc<cliamp::Client<SharedTransport<T>>>>,
    /// The shared transport both clients fetch through, kept so
    /// [`Self::with_host`] can rebuild the directory client.
    transport: Option<SharedTransport<T>>,
    /// Where `radio.toml` lives; `None` means preferences live in memory
    /// for this session only.
    store_path: Option<PathBuf>,
    /// The store as last read or last mutated in the view.
    store: RadioStore,
    /// Whether the store file could not be read: the view then works on an
    /// empty store and saving is refused for the whole session, so a file
    /// that was never seen is never overwritten.
    store_unreadable: bool,
    /// The country the locale yields, while the store has no answer (S5).
    detected: Option<String>,
    /// The `/countries` index, once loaded.
    countries: Vec<Country>,
    /// The home country's `/states/<home>/` index, once loaded (T6d).
    states: Vec<State>,
    /// The `/tags` index, once loaded.
    tags: Vec<Tag>,
    /// The place whose stations were already requested this session, set
    /// when the request starts: an answer — empty, failed or stale — is
    /// never re-requested behind the user's back; `Ctrl+R` is the retry.
    requested: Option<Place>,
    /// The last station answer and the place it belongs to, set only when
    /// an answer lands: coming back to its place shows it again without a
    /// request (S9), and it renders only in that exact place.
    catalog: Option<(Place, Vec<Station>)>,
    /// Whether `/countries` was already requested (or answered) this
    /// session: an empty answer is still an answer.
    countries_loaded: bool,
    /// Whether `/states/<home>/` was already requested this session.
    states_loaded: bool,
    /// Whether `/tags` was already requested this session.
    tags_loaded: bool,
    /// Whether the cliamp channels were already requested (or answered)
    /// this session (issue #48, S2): an empty answer is still an answer.
    cliamp_channels_loaded: bool,
    /// Whether the cliamp listeners were already requested this session.
    cliamp_stats_loaded: bool,
    /// The cliamp channels, as `/stations` answered.
    cliamp_channels: Vec<Channel>,
    /// The cliamp listeners, as `/statistics` answered.
    cliamp_listeners: Vec<Listeners>,
    /// The last cliamp songs answer and the place it belongs to, set only
    /// when an answer lands: coming back to its place shows it again
    /// without a request (S9).
    cliamp_songs: Option<(Place, Vec<Song>)>,
    /// Whether the cliamp section shows at all (issue #48, S1): the
    /// `[radio] cliamp` switch, shown unless the config hides it.
    cliamp_enabled: bool,
    /// The request in flight, its answer still to be polled.
    pending: Option<Receiver<Outcome>>,
    /// The status-line message waiting to be taken.
    message: Option<String>,
}

impl<T: Transport + Send + Sync + 'static> RadioData<T> {
    /// A wired radio over the given transport, store file and the country
    /// the locale yields (S5) — computed by the caller, usually
    /// [`crate::radio::country::detect`]. Both clients — the directory's
    /// and the cliamp service's (issue #48) — are built over the one
    /// transport. Loading the store here reads one small file and never
    /// touches the network; a corrupt file becomes the first status-line
    /// message, and the file is left exactly as it is.
    pub fn new(
        transport: Option<T>,
        store_path: Option<PathBuf>,
        detected: Option<String>,
    ) -> Self {
        let mut message = None;
        let store = match store_path.as_deref() {
            Some(path) => match RadioStore::load(path) {
                Ok(store) => store,
                Err(error) => {
                    message = Some(format!("velo: {error}"));
                    RadioStore::default()
                }
            },
            None => RadioStore::default(),
        };
        let shared = transport.map(SharedTransport::from_transport);
        Self {
            client: shared
                .clone()
                .map(|transport| Arc::new(Client::new(transport))),
            cliamp: shared
                .clone()
                .map(|transport| Arc::new(cliamp::Client::new(transport))),
            transport: shared,
            store_unreadable: message.is_some(),
            store_path,
            store,
            detected,
            countries: Vec::new(),
            states: Vec::new(),
            tags: Vec::new(),
            catalog: None,
            requested: None,
            countries_loaded: false,
            states_loaded: false,
            tags_loaded: false,
            cliamp_channels_loaded: false,
            cliamp_stats_loaded: false,
            cliamp_channels: Vec::new(),
            cliamp_listeners: Vec::new(),
            cliamp_songs: None,
            cliamp_enabled: true,
            pending: None,
            message,
        }
    }

    /// Rebuilds the directory client at `host`, the host the config names
    /// ([`crate::config::radio_host_from`], issue #14). The cliamp client
    /// is untouched: its host is the service's constant, not the
    /// directory's (issue #48, S5).
    #[must_use]
    pub fn with_host(mut self, host: &str) -> Self {
        if let Some(transport) = &self.transport {
            self.client = Some(Arc::new(Client::with_host(
                SharedTransport(Arc::clone(&transport.0)),
                host,
            )));
        }
        self
    }

    /// Sets whether the cliamp section shows (issue #48, S1): the
    /// `[radio] cliamp` switch; shown unless the config says `false`.
    #[must_use]
    pub fn with_cliamp(mut self, cliamp: bool) -> Self {
        self.cliamp_enabled = cliamp;
        self
    }

    /// Collects a finished request, if one arrived. A station answer for a
    /// place the view has left is cached, not shown.
    fn collect(&mut self, view: &mut RadioView) {
        let Some(receiver) = &self.pending else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            // A thread that died without answering is a failure, never a
            // request that runs forever (the indexing rule).
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.message = Some("the radio request stopped without an answer".to_owned());
                // The error is the wait's end too (S10).
                view.set_loading(None);
                return;
            }
        };
        self.pending = None;
        // An answer or an error landed: the waiting fact goes away with
        // the request (S10).
        view.set_loading(None);
        match outcome {
            Outcome::Countries(result) => match result {
                Ok(countries) => {
                    self.countries.clone_from(&countries);
                    view.set_countries(countries);
                }
                Err(error) => self.message = Some(format!("velo: {error}")),
            },
            Outcome::States(result) => match result {
                Ok(states) => {
                    self.states.clone_from(&states);
                    view.set_states(states);
                }
                Err(error) => self.message = Some(format!("velo: {error}")),
            },
            Outcome::Tags(result) => match result {
                Ok(tags) => {
                    self.tags.clone_from(&tags);
                    view.set_tags(tags);
                }
                Err(error) => self.message = Some(format!("velo: {error}")),
            },
            Outcome::Stations { place, result } => match result {
                Ok(stations) => {
                    // The cache keeps the answer under its own place; the
                    // view takes it only when it still stands there (S9).
                    if view.place() == &place {
                        view.set_catalog(place.clone(), stations.clone());
                    }
                    self.catalog = Some((place, stations));
                }
                Err(error) => self.message = Some(format!("velo: {error}")),
            },
            Outcome::CliampStations(result) => match result {
                Ok(channels) => {
                    self.cliamp_channels.clone_from(&channels);
                    view.set_cliamp_channels(channels);
                }
                // One status line naming the reason; the section stays
                // present but empty (issue #48, S4).
                Err(error) => self.message = Some(format!("velo: {error}")),
            },
            Outcome::CliampStatistics(result) => match result {
                Ok(listeners) => {
                    self.cliamp_listeners.clone_from(&listeners);
                    view.set_cliamp_listeners(listeners);
                }
                // Only the `●` counts drop: the channels were set already
                // and stay listed, and the failure is said once (S4).
                Err(error) => self.message = Some(format!("velo: {error}")),
            },
            Outcome::CliampSongs { place, result } => match result {
                Ok(songs) => {
                    if view.place() == &place {
                        view.set_cliamp_songs(place.clone(), songs.clone());
                    }
                    self.cliamp_songs = Some((place, songs));
                }
                Err(error) => self.message = Some(format!("velo: {error}")),
            },
        }
    }

    /// Saves a mutated store. Refused outright when the file was never
    /// readable; a failed write reports on the status line while the change
    /// stays in memory for the next save to carry.
    fn save(&mut self) {
        if self.store_unreadable {
            return;
        }
        let Some(path) = &self.store_path else {
            return;
        };
        if let Err(error) = self.store.save(path) {
            self.message = Some(format!("velo: {error}"));
        }
    }

    /// Starts the request the view's current place still needs, if any and
    /// if the single slot is free. The loaded markers are set when the
    /// request starts, so an answer — including an empty or a failed one —
    /// is not re-requested behind the user's back; `Ctrl+R` is the retry.
    fn request(&mut self, view: &mut RadioView) {
        if self.pending.is_some() || self.client.is_none() {
            return;
        }
        let place = view.place().clone();
        match place {
            Place::Countries if !self.countries_loaded => {
                self.countries_loaded = true;
                let client = Arc::clone(self.client.as_ref().expect("just checked"));
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    // The interface may already be gone; nobody is left to tell.
                    let _ = sender.send(Outcome::Countries(client.countries()));
                });
                self.pending = Some(receiver);
                // The waiting fact goes up with the request (S10) and comes
                // down when the answer or the error lands.
                view.set_loading(Some(Place::Countries));
            }
            // The home country's state index (T6d, S5), fetched beside the
            // country index and only for the home country: the directory
            // records a region for roughly 44% of stations, so the whole
            // world's regions are not worth the request. With no home
            // country — unanswered or declined — nothing changes.
            Place::Countries if !self.states_loaded => {
                let Some(home) = self.store.country.clone().filter(|code| code != "none") else {
                    return;
                };
                self.states_loaded = true;
                let client = Arc::clone(self.client.as_ref().expect("just checked"));
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    let _ = sender.send(Outcome::States(client.states(&home)));
                });
                self.pending = Some(receiver);
                view.set_loading(Some(Place::Countries));
            }
            Place::Tags if !self.tags_loaded => {
                self.tags_loaded = true;
                let client = Arc::clone(self.client.as_ref().expect("just checked"));
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    let _ = sender.send(Outcome::Tags(client.tags()));
                });
                self.pending = Some(receiver);
                view.set_loading(Some(Place::Tags));
            }
            // The cliamp section (issue #48, S2, S4): `/stations` first,
            // `/statistics` right after its answer lands — one request in
            // flight at a time, two in total per open, and none on a timer
            // (S4). A failure marks its own loaded flag, so it is never
            // re-requested behind the user's back; `Ctrl+R` is the retry.
            Place::Cliamp if !self.cliamp_channels_loaded => {
                self.cliamp_channels_loaded = true;
                let client = Arc::clone(self.cliamp.as_ref().expect("just checked"));
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    let _ = sender.send(Outcome::CliampStations(client.stations()));
                });
                self.pending = Some(receiver);
                view.set_loading(Some(Place::Cliamp));
            }
            Place::Cliamp if !self.cliamp_stats_loaded => {
                self.cliamp_stats_loaded = true;
                let client = Arc::clone(self.cliamp.as_ref().expect("just checked"));
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    let _ = sender.send(Outcome::CliampStatistics(client.statistics()));
                });
                self.pending = Some(receiver);
                view.set_loading(Some(Place::Cliamp));
            }
            Place::CliampChannel { ref id, .. } if self.requested.as_ref() != Some(&place) => {
                self.spawn_cliamp_songs(place.clone(), id.clone());
                view.set_loading(Some(place));
            }
            place => {
                if requests_stations(&place) && self.requested.as_ref() != Some(&place) {
                    self.spawn_stations(place.clone(), view.order());
                    view.set_loading(Some(place));
                }
            }
        }
    }

    /// Starts the station search of one place on the worker thread. The
    /// answer goes through [`loadable_stations`], which caps it at 200 and
    /// drops the rows without a stream URL.
    fn spawn_stations(&mut self, place: Place, order: Order) {
        self.requested = Some(place.clone());
        let client = Arc::clone(self.client.as_ref().expect("a client is present"));
        let search = station_search(&place, order);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = client.search_stations(&search).map(loadable_stations);
            let _ = sender.send(Outcome::Stations { place, result });
        });
        self.pending = Some(receiver);
    }

    /// Starts one cliamp channel's songs request on the worker thread
    /// (issue #48, S2): one GET of `/<id>/tracks`, the songs in the
    /// service's order.
    fn spawn_cliamp_songs(&mut self, place: Place, id: String) {
        self.requested = Some(place.clone());
        let client = Arc::clone(self.cliamp.as_ref().expect("a client is present"));
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = client.songs(&id);
            let _ = sender.send(Outcome::CliampSongs { place, result });
        });
        self.pending = Some(receiver);
    }

    /// Hands the view the cached answer when it stands in the answer's
    /// place without holding it: coming back to a loaded place shows the
    /// cache again and never re-requests it (S9). The answer's own place
    /// is the only place it may show in — never the top, a sibling country
    /// or a tag.
    fn sync(&mut self, view: &mut RadioView) {
        if self.pending.is_some() {
            return;
        }
        if requests_stations(view.place()) {
            if let Some((place, stations)) = &self.catalog
                && place == view.place()
                && view.catalog_place().is_none_or(|held| held != place)
            {
                view.set_catalog(place.clone(), stations.clone());
            }
            return;
        }
        // A cliamp channel's songs behave the same way (issue #48): the
        // cached answer is handed back only in its own place.
        if matches!(view.place(), Place::CliampChannel { .. })
            && let Some((place, songs)) = &self.cliamp_songs
            && place == view.place()
            && view.cliamp_songs_place().is_none_or(|held| held != place)
        {
            view.set_cliamp_songs(place.clone(), songs.clone());
        }
    }

    /// Re-requests only what the current place needs, dropping the loaded
    /// markers for it first. At the top of the view there is nothing to
    /// refresh. While a request is in flight the ask is spent on the
    /// running one.
    fn refresh(&mut self, view: &mut RadioView) {
        match view.place() {
            // The top and the no-country place (S4) hold nothing the
            // directory answered: there is nothing to refresh.
            Place::Top | Place::NoCountry => return,
            Place::Countries => {
                self.countries_loaded = false;
                self.states_loaded = false;
            }
            Place::Tags => self.tags_loaded = false,
            // The cliamp section asks both requests again (issue #48, S4);
            // one channel's songs re-ask like a place's stations do.
            Place::Cliamp => {
                self.cliamp_channels_loaded = false;
                self.cliamp_stats_loaded = false;
            }
            Place::CliampChannel { .. }
            | Place::Country { .. }
            | Place::Tag { .. }
            | Place::Search { .. } => {
                self.requested = None;
            }
        }
        self.request(view);
    }
}

impl<T: Transport + Send + Sync + 'static> RadioWire for RadioData<T> {
    fn view(&self) -> RadioView {
        // The stored country decides the row: a code is the near-you row,
        // `none` declined it, `None` was never asked (S5).
        let own_country = self.store.country.clone().filter(|code| code != "none");
        let mut view = RadioView::new(
            self.store.clone(),
            self.countries.clone(),
            self.tags.clone(),
            own_country,
        );
        // The browse rows exist before their indexes load, so the sections
        // can be entered at all; the indexes then fill them in.
        view.show_browse_rows();
        if self.store.country.is_none() {
            view.ask_country(self.detected.clone());
        }
        // The regions of the home country, once their index is fetched
        // (T6d): rows beside the places at the top.
        if !self.states.is_empty() {
            view.set_states(self.states.clone());
        }
        // The cliamp section's channels and listeners, once fetched (issue
        // #48, S2): a fresh view stands at the top, and the section's fact
        // names the channels when there are any. A failed request is an
        // empty answer like an empty index is: the section stays.
        view.set_cliamp_enabled(self.cliamp_enabled);
        if self.cliamp_channels_loaded {
            view.set_cliamp_channels(self.cliamp_channels.clone());
        }
        if self.cliamp_stats_loaded {
            view.set_cliamp_listeners(self.cliamp_listeners.clone());
        }
        // The station cache is deliberately not injected here (S9): a
        // fresh view stands at the top, and the top is a section list —
        // never a country's stations. [`Self::sync`] hands the cache back
        // when the view stands in the answer's own place again.
        view
    }

    fn poll(&mut self, view: &mut RadioView) {
        self.collect(view);
        if view.take_dirty() {
            self.store = view.store().clone();
            self.save();
        }
        if view.take_refresh_request() {
            self.refresh(view);
            return;
        }
        self.sync(view);
        self.request(view);
    }

    fn take_message(&mut self) -> Option<String> {
        self.message.take()
    }

    fn loading(&self) -> bool {
        self.pending.is_some()
    }
}

impl RadioData<HttpTransport> {
    /// The shell's radio wiring: the real transport, the directory host
    /// and the cliamp switch from the real config, and the country from
    /// the real environment (`LC_ALL`, then `LC_MESSAGES`, then `LANG`).
    /// `store_path` is the `radio.toml` beside the config file, `host` is
    /// the directory host the config names
    /// ([`crate::config::radio_host_from`]) and `cliamp` is the `[radio]
    /// cliamp` switch ([`crate::config::radio_cliamp_from`], issue #48).
    ///
    /// A transport that cannot be built (none is known to fail today)
    /// leaves the view usable without a directory instead of failing the
    /// interface.
    #[must_use]
    pub fn open(store_path: Option<PathBuf>, host: &str, cliamp: bool) -> Self {
        Self::new(
            HttpTransport::new().ok(),
            store_path,
            crate::radio::country::detect(|key| std::env::var(key).ok()),
        )
        .with_host(host)
        .with_cliamp(cliamp)
    }
}
