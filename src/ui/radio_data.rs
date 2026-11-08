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
//! place on screen needs. An index already loaded is reused; a failed
//! request becomes one status-line message, never a retry behind the
//! user's back.
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

use crate::radio::client::{
    self, Client, Country, HttpTransport, Order, Station, StationSearch, Tag, Transport,
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

/// What one finished background request hands back.
enum Outcome {
    /// The `/countries` index.
    Countries(Result<Vec<Country>, client::Error>),
    /// The `/tags` index.
    Tags(Result<Vec<Tag>, client::Error>),
    /// The stations of one place, capped by [`loadable_stations`].
    Stations {
        /// The place the search was for, so a stale answer is dropped.
        key: CatalogKey,
        /// The search's answer.
        result: Result<Vec<Station>, client::Error>,
    },
}

/// Which place the one catalog slot holds, or the request in flight is
/// for. Country and region together; a tag by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CatalogKey {
    /// A country, optionally one of its regions.
    Country {
        /// ISO 3166-1 code.
        code: String,
        /// Region within the country; empty for the country itself.
        state: String,
    },
    /// A tag.
    Tag {
        /// The tag's name.
        name: String,
    },
}

impl CatalogKey {
    /// The search a place's stations load with: the country code, the
    /// region when the place names one, or the exact tag, in the view's
    /// order, at the directory's own 200-per-place cap.
    fn search(&self, order: Order) -> StationSearch {
        match self {
            Self::Country { code, state } => StationSearch {
                countrycode: Some(code.clone()),
                state: (!state.is_empty()).then(|| state.clone()),
                order,
                ..StationSearch::default()
            },
            Self::Tag { name } => StationSearch {
                tag: Some(name.clone()),
                order,
                ..StationSearch::default()
            },
        }
    }
}

/// The [`CatalogKey`] of the place the view stands in, if it is a place.
fn place_key(place: &Place) -> Option<CatalogKey> {
    match place {
        Place::Country { code, state, .. } => Some(CatalogKey::Country {
            code: code.clone(),
            state: state.clone(),
        }),
        Place::Tag { name } => Some(CatalogKey::Tag { name: name.clone() }),
        Place::Top | Place::Countries | Place::Tags => None,
    }
}

/// The radio data: the client, the store and its file, and the one request
/// channel in flight. Build with [`RadioData::open`] for the shell or
/// [`RadioData::new`] with an injected transport for the tests; hand it to
/// the shell as a [`RadioWire`].
pub struct RadioData<T> {
    /// The directory client; `None` when the transport could not be built,
    /// in which case every request is skipped and the view still works.
    client: Option<Arc<Client<T>>>,
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
    /// The `/tags` index, once loaded.
    tags: Vec<Tag>,
    /// The stations of the one catalog slot, once loaded.
    catalog: Vec<Station>,
    /// The place [`Self::catalog`] belongs to.
    catalog_key: Option<CatalogKey>,
    /// Whether `/countries` was already requested (or answered) this
    /// session: an empty answer is still an answer.
    countries_loaded: bool,
    /// Whether `/tags` was already requested this session.
    tags_loaded: bool,
    /// The request in flight, its answer still to be polled.
    pending: Option<Receiver<Outcome>>,
    /// The status-line message waiting to be taken.
    message: Option<String>,
}

impl<T: Transport + Send + Sync + 'static> RadioData<T> {
    /// A wired radio over the given client, store file and the country
    /// the locale yields (S5) — computed by the caller, usually
    /// [`crate::radio::country::detect`]. Loading the store here reads one
    /// small file and never touches the network; a corrupt file becomes
    /// the first status-line message, and the file is left exactly as it
    /// is.
    pub fn new(
        client: Option<Client<T>>,
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
        Self {
            client: client.map(Arc::new),
            store_unreadable: message.is_some(),
            store_path,
            store,
            detected,
            countries: Vec::new(),
            tags: Vec::new(),
            catalog: Vec::new(),
            catalog_key: None,
            countries_loaded: false,
            tags_loaded: false,
            pending: None,
            message,
        }
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
                return;
            }
        };
        self.pending = None;
        match outcome {
            Outcome::Countries(result) => match result {
                Ok(countries) => {
                    self.countries.clone_from(&countries);
                    view.set_countries(countries);
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
            Outcome::Stations { key, result } => match result {
                Ok(stations) => {
                    self.catalog.clone_from(&stations);
                    self.catalog_key = Some(key.clone());
                    if place_key(view.place()).as_ref() == Some(&key) {
                        view.set_catalog(stations);
                    }
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
        match view.place() {
            Place::Countries if !self.countries_loaded => {
                self.countries_loaded = true;
                let client = Arc::clone(self.client.as_ref().expect("just checked"));
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    // The interface may already be gone; nobody is left to tell.
                    let _ = sender.send(Outcome::Countries(client.countries()));
                });
                self.pending = Some(receiver);
            }
            Place::Tags if !self.tags_loaded => {
                self.tags_loaded = true;
                let client = Arc::clone(self.client.as_ref().expect("just checked"));
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    let _ = sender.send(Outcome::Tags(client.tags()));
                });
                self.pending = Some(receiver);
            }
            place => {
                if let Some(key) = place_key(place)
                    && self.catalog_key.as_ref() != Some(&key)
                {
                    self.spawn_stations(key, view.order());
                }
            }
        }
    }

    /// Starts the station search of one place on the worker thread. The
    /// answer goes through [`loadable_stations`], which caps it at 200 and
    /// drops the rows without a stream URL.
    fn spawn_stations(&mut self, key: CatalogKey, order: Order) {
        self.catalog_key = Some(key.clone());
        let client = Arc::clone(self.client.as_ref().expect("a client is present"));
        let search = key.search(order);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = client.search_stations(&search).map(loadable_stations);
            let _ = sender.send(Outcome::Stations { key, result });
        });
        self.pending = Some(receiver);
    }

    /// Re-requests only what the current place needs, dropping the loaded
    /// markers for it first. At the top of the view there is nothing to
    /// refresh. While a request is in flight the ask is spent on the
    /// running one.
    fn refresh(&mut self, view: &mut RadioView) {
        match view.place() {
            Place::Top => return,
            Place::Countries => self.countries_loaded = false,
            Place::Tags => self.tags_loaded = false,
            Place::Country { .. } | Place::Tag { .. } => self.catalog_key = None,
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
        if !self.catalog.is_empty() {
            view.set_catalog(self.catalog.clone());
        }
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
    /// The shell's radio wiring: the real transport and the country from
    /// the real environment (`LC_ALL`, then `LC_MESSAGES`, then `LANG`).
    /// `store_path` is the `radio.toml` beside the config file.
    ///
    /// A transport that cannot be built (none is known to fail today)
    /// leaves the view usable without a directory instead of failing the
    /// interface.
    #[must_use]
    pub fn open(store_path: Option<PathBuf>) -> Self {
        Self::new(
            HttpTransport::new().ok().map(Client::new),
            store_path,
            crate::radio::country::detect(|key| std::env::var(key).ok()),
        )
    }
}
