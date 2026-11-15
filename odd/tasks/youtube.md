# youtube — a result you can play, resolved by `yt-dlp` (issue #70)

Search YouTube from the sources screen, pick a result, play it. velo spawns
`yt-dlp`, reads its output, and never talks to YouTube itself. What comes back
is a queue entry with a title, an artist, a length, a position and a seek, so
the queue, the history, the media keys and the `● live`/pause vocabulary keep
working unchanged.

Design only. No code is written before this document is agreed.

## Specs

**S1 — a `YouTube` row, honest about the tool.**
"A new row in `Sources` (`YouTube`)" — the first of the three shapes the issue
names, because the screen is already the map. `yt-dlp` "is detected at launch
(`which yt-dlp`), with a status line that says how to install it when a YouTube
row is opened without it". ffmpeg is not detected separately: it joins only if
`yt-dlp` itself reports needing it, and the failure names it.

**S2 — the search asks for JSON, not for a flat list to scrape.**
"`yt-dlp "ytsearch10:<query>"` answers a flat list, or the app asks for JSON
and renders it." velo asks for JSON: one object per line, parsed, never a
stdout scrape with a regex over a human format.

**S3 — a result is a queued entry with a body.**
"the result becomes a queued entry with a title, an artist, a length, a
position and a seek, so the queue, the history, the media keys and the
`● live`/pause vocabulary all keep working." A result is finite, so it is not
a `Station`: it has a duration and it seeks. A result with no duration (a live
broadcast) keeps playing with its seek disabled and says so, instead of
inventing a length.

**S4 — opened when it plays, not when it was queued.** *(revised: see L9)*
"A queued item has to be re-resolved when it plays, not when it was queued."
The queue holds the video's identity; the stream is opened at the moment
playback reaches the entry, every time, and **the tool holds the stream rather
than handing velo a URL** — measured, the only format whose URL velo may fetch
is the one it cannot decode (L9). A seek is therefore a fresh open, not a
position inside a body.

**S5 — progress and failure that name the reason.**
"Progress reporting that is honest: "resolving…" while `yt-dlp` works, a
failure that names the reason, and no silent hang." The status line says what is
happening and, on failure, which sentence of `yt-dlp`'s own output explains it.

**S6 — one bounded process, off the event loop.**
"One spawned process per resolve and per play, with a timeout and a kill, off
the event loop". A process that outlives its deadline is killed with its
children, and the interface gets a status line — never a spinner that spins
forever and never a frozen screen.

**S7 — nothing about the search is stored.**
"Nothing about the user's searches stored, consistent with `## Privacy` in the
README." The query lives in memory only. A played entry reaches the history the
way every played thing does, carrying its title, its uploader and its identity —
that is the history's existing behaviour, not a new log.

**S8 — the README says what leaves the machine.**
"A README section that says plainly what leaves the machine when this source is
used, and that removing the row or the tool removes the feature." `##
Requirements` lists `yt-dlp` as optional, and `## Privacy` names it beside the
directory, the cliamp service and the Subsonic server.

**S9 — the loudness path refuses, it does not pretend.**
"ReplayGain, bit-perfect and the loudness path do not apply to a decoded stream
and must say so instead of pretending." A resolved entry reports itself as
out of scope for gain, bit-perfect and loudness in one line, and nothing in
those paths silently treats it as a normal track.

**S10 — velo runs unchanged without the tool.**
"`yt-dlp` is not in the repository and never will be: it is an optional external
tool". No build dependency, no launch failure, no row that opens a broken
screen: with no `yt-dlp`, everything else behaves exactly as it does today.

## Design

### Where it plugs in, and why almost nothing is new

`Entry` (`src/audio/output.rs:388`) is already typed for exactly this: `File`
and `Station { url, name }`, with `as_path()` answering `None` for the variant
that must not be opened as a file. `PathResolver`
(`src/audio/output.rs:450`) already answers three questions — `resolve`
(a file), `cached` (would reaching this stall), `open_streaming` (an
already-opened source for a URL) — and `src/ui/mod.rs:4472`
(`is_uncached_remote`) already sends every entry whose `cached` is `false` to a
worker thread (`start_as_pending`, `:4525`) whose answer arrives by `mpsc` and
is turned into the `⟳ fetching ` status line by `poll_pending_resolve`
(`:4569`). The radio work built that seam; a YouTube entry is its second
consumer, not a new one.

So the entry declares the truth: **`cached` is always `false`** (a URL that
expires was never cached), and the resolver's `open_streaming` does the work.

```text
youtube://<video-id>  ──cached()=false──▶  resolve worker
                                            └─▶ yt-dlp -g  ──▶ stream URL
                                                             └─▶ radio's HTTP open
                                                             └─▶ Box<dyn MediaSource>
```

The last hop is the point: the URL `yt-dlp` returns is opened by the **same**
streaming machinery radio uses (`src/radio/stream.rs:529`,
`pub fn open(url, Timeouts)`, whose connect/response/per-read bounds are already
`src/radio/stream.rs:79`/`:98`). No second streaming path, no second decoder
stack, no `ureq` call written twice.

### Module

```text
src/youtube/mod.rs       capability: detect() -> Option<PathBuf>; pub use; ToolError
src/youtube/tool.rs      the bounded child runner: run(program, args, Deadline)
src/youtube/search.rs    query -> Vec<Found>, miniserde over the JSON lines
src/youtube/resolve.rs   video id -> audio URL, and the PathResolver impl
```

Named to sit beside `src/radio/{client,stream,store,lists,data}.rs` and
`src/subsonic/{client,cache,credentials}.rs`.

### The bounded child runner — the one genuinely new machine

Nothing in the tree captures a child's stdout, imposes a deadline or kills a
stuck child. `open_url` (`src/ui/mod.rs:565`) is `spawn().map(|_| ())`:
fire-and-forget. This is the piece to get right once.

```rust
pub struct Deadline(Duration);

pub fn run(program: &Path, args: &[OsString], deadline: Deadline)
    -> Result<String, ToolError>;   // stdout, on success
```

Rules it must satisfy:

1. **argv, never a shell.** `Command::new(program).args(args)`. The query and
   the video id are separate `OsString` arguments; neither is ever interpolated
   into a string that a shell would re-split. A query holding quotes, `;` or a
   leading `-` is a query, not a command. `--` terminates options before the
   query is passed.
2. **No inherited terminal.** stdin null, stdout and stderr piped: the child can
   never read a keystroke meant for the interface, and can never paint over it.
3. **Both pipes drained.** stdout on a reader thread from the start; a child
   that fills a pipe blocks forever otherwise, which is a hang we would have
   built ourselves. stderr is read to a bounded tail (its first line is what an
   error message quotes).
4. **One deadline, then a kill of the whole group.** The child is spawned with
   `process_group(0)` (`std::os::unix::process::CommandExt`) and the deadline
   kills the *group*: `yt-dlp` runs ffmpeg and a plain `kill` would leave the
   grandchild holding the pipe and the CPU. `rustix` is already a dependency for
   `kill_process` (`src/bin/velo.rs:28`); this adds `kill_process_group`, no new
   crate.
5. **Always reaped.** After the kill, `wait()` runs unconditionally: a zombie is
   not an acceptable outcome of a failure path.
6. **Deadlines as constants, with a reason.** Search 20 s (ten flat entries), a
   play-time resolve 30 s (one full extraction). Both are named constants beside
   the radio's `connect`/`response`/`read` bounds, with the same kind of comment.
7. **Deterministic exit mapping.** Non-zero status → its first stderr line;
   deadline → `ToolError::Deadline`; the program not found at `spawn` →
   `ToolError::NotInstalled`. A success with no bytes is `Ok("")`, not an
   error: an empty search answer is "no results" and an empty resolve is
   "the tool answered nothing", which are the callers' sentences to say —
   the runner does not invent a failure the tool did not report.

The runner is injectable, mirroring `set_url_opener` (`tests/ui_radio.rs:1878`):
the resolver holds a program path, so a test constructs it with a fake binary
and no test ever reaches the network.

### Wire formats

**Search.** One process, flat extraction, JSON per line:

```text
yt-dlp --flat-playlist --dump-json --no-warnings --no-progress
       --playlist-end 10 -- ytsearch10:<query>
```

`--flat-playlist` keeps it to the search page's own data instead of extracting
ten videos one by one (which is ten network round trips and minutes of waiting).
Each line is one object; velo reads only
`id`, `title`, `uploader` (falling back to `channel`), `duration` (seconds,
`null` for a live broadcast) and ignores the rest. Absent or unparsable lines
are skipped, and an empty list says "no results", never "0 of 0".

Parsing uses **`miniserde`**, the crate the repo already chose over `serde_json`
for exactly this job (`Cargo.toml`: its extra `PartialEq<Value>` impl breaks
inference in existing tests). No `serde_json` dependency is added.

**Resolve at play time.** One process, the URL only:

```text
yt-dlp --no-playlist --no-warnings --no-progress --get-url
       --format bestaudio[ext=m4a]/bestaudio[ext=mp3]/bestaudio
       -- https://www.youtube.com/watch?v=<id>
```

`--get-url` prints the media URL on one line, and that line is what the radio's
`open` receives. `--no-playlist` is not cosmetic: a watch URL carrying `&list=`
would otherwise resolve the whole playlist and print dozens of lines.

The format selector is deliberate and belongs in the document, not in a commit
message: the streaming decoder is symphonia with `mp3`, `aac`, `flac`, `vorbis`,
`ogg`, `wav`, `isomp4` — **there is no Matroska/WebM demuxer**, so `bestaudio`'s
default (`webm`/opus) would be a stream velo cannot decode. `ext=m4a` (AAC in
MP4) plays through the path that already exists; `mp3` is the first fallback.
Everything else fails with the tool's own sentence, which names the format it
could not use rather than hanging on a stream nobody can read. Adding a WebM
demuxer is a separate issue, if it ever matters.

**Identity.** `youtube://<video-id>`, the twin of `subsonic://<host>/<id>`
(`src/subsonic/cache.rs:92`, `is_pseudo_path`). A queued entry carries the
pseudo-path *and* the display body (title, uploader, duration), so `##
History`, `## Favourites` and playlists need no new format: their `path` field
holds the pseudo-path untouched, exactly as it already holds a `subsonic://`
row, and their `artist`/`title` fields hold what the search answered. A `path`
that says `youtube://` but carries no id is refused with a sentence, never
opened as a file.

**Capability.** `detect()` scans `PATH` for an executable `yt-dlp`
(`yt-dlp.exe` on Windows) — a file scan, not a spawn: launching velo must not
cost a process. It answers `Option<PathBuf>`, is resolved once beside
`subsonic_seams` (`src/bin/velo.rs:160`), and a missing tool installs the
degraded resolver of `subsonic/cache.rs:689` (`Unconfigured`): local files pass
through, `youtube://` fails with the install sentence. That is what keeps S10
true — the config, the queue and the resolver stay valid with no tool installed,
and nothing is refused except the one thing that genuinely cannot work.

**What velo never asks for.** No `--cookies-from-browser`, ever, and no
cookies file: it would hand the user's browser session to a second program for
the price of logging in. No `--write-*`, no output template, no writes to disk:
the tool resolves, velo plays. No `--update` (`yt-dlp` updates itself, and that
is the user's business).

### The screen

`Source::Youtube` joins `Source::ALL` (`src/ui/sources.rs:36`) directly after
`Radio`, with the label `YouTube`, and `open_source` (`src/ui/mod.rs:1834`)
gets one arm. Its row shows a fact — `yt-dlp` found, or not — from the PATH scan
alone, no spawn, no version check at launch.

The screen is one query line and a list of results: typed characters go to the
query, `enter` searches, `enter` on a result queues and plays it, `esc` goes
back, `j`/`k`/arrows move with the wrapping the sources screen already has. The
status line follows the radio's vocabulary: `⟳ searching ` while the process
runs (`const FETCHING_LINE_PREFIX: &str = "⟳ fetching "`,
`src/ui/mod.rs:83`, gets a sibling constant), the reason on failure, and the
loudness note of S9 on the first play of a resolved entry.

S3's live case is not a new mode: a result with `duration: null` plays, shows
elapsed time only, and refuses a seek with a sentence — the vocabulary the
station rows already taught (`live_row`, `src/ui/mod.rs:6822`).

### Error sentences (UI copy, English)

| Situation | Status line |
| --- | --- |
| no tool | `yt-dlp is not installed — install it to play YouTube (brew install yt-dlp, or your package manager)` |
| deadline | `yt-dlp did not answer within 20s — the request was cancelled` |
| tool failed | `yt-dlp could not answer: <its first stderr line> (exit N)` |
| broken row | `<path> is not a YouTube entry: <why>` |
| asked for a file | `<path> is a YouTube entry — velo plays it as a resolved stream, and it is never a file` |
| cannot be seeked | `this YouTube entry cannot be played as a ranged stream — there is no file to fall back to` |
| no results | `no results for that query` |
| a live result, `seek` | `velo: nothing to seek in a live YouTube stream` |

Each names the reason and the next step; none of them is a spinner. A line
nobody can use is skipped and an unusable ones-and-zeros answer is "no
results", so the runner needs no sentence for either: the caller already has
one.

### Tests, with no network anywhere

`tests/youtube.rs`, shaped like `tests/remote_stream.rs`:

1. **Runner.** A fake binary written into a temp dir (a `sh` script): answers
   stdout → the string comes back; exits 3 with stderr → the first line is the
   error; sleeps past a 200 ms deadline → `Deadline` *and the process is gone*
   (asserted through `try_wait` on the pid); spawns a grandchild that ignores
   the pipe → the group kill takes the grandchild too.
2. **Search.** Canned JSON lines from the fake: two results parsed with title,
   uploader and duration; a `duration: null` row; a `channel` instead of
   `uploader`; a garbage line skipped; empty output → `no results`.
3. **Identity.** `youtube://` round-trips through the queue and the history file
   untouched, and a `youtube://` with no id is refused with a sentence.
4. **UI.** The row and its fact (found / not found), the `⟳ searching ` line
   while the fake sleeps, and the failure sentence — through the injected
   resolver, the way `tests/ui_cliamp.rs:1017` pins the fetching line.
5. **No tool.** With `detect()` answering `None`, the local library, radio and
   the rest of the sources behave exactly as they do today (S10).

## Delivery slices

This feature is one seam but not one reviewable diff: the runner, the parse, the
queue kind and the screen are four things a reviewer reads for four different
reasons. The branch delivers them as chained slices, each its own commit, and a
PR takes them in this order.

| Slice | Files | What a reviewer is checking |
| --- | --- | --- |
| A — the machine | `src/youtube/tool.rs` | argv only, one deadline, the group kill, nothing left unreaped |
| B — the tool and the search | `src/youtube/mod.rs`, `src/youtube/search.rs` | the PATH scan spawning nothing, the pseudo-path guard, one JSON line per result |
| C — the entry and the resolver | `src/audio/output.rs`, `src/youtube/resolve.rs`, `src/ui/mod.rs` | every `Entry` match answered, the carried source, the format selector, S9's refusal |
| D — the screen | `src/ui/sources.rs`, `src/ui/mod.rs`, `src/ui/youtube.rs` | the row and its fact, the query line, the status vocabulary |
| E — the README | `README.md` | `## Requirements` and `## Privacy` say what leaves the machine |

Slices A–C carry one shared test file (`tests/youtube.rs`), which is why they
land in one commit: an integration test referencing a type that does not exist
yet would leave the tree uncompilable between slices. D and E are separate
commits, and D is the only one that changes a screen.

Landed on `feat/youtube`, in this order: `997751e` this document, `77f8e90`
slices A–C, `0400db9` slice D, `d105b4d` slice E.

## Tasks

| # | Task | Spec | Route |
| --- | --- | --- | --- |
| T1 | `src/youtube/tool.rs`: the bounded runner, deadline, group kill, reaping, exit mapping | S6 | worker |
| T2 | `src/youtube/mod.rs`: `detect()`, `ToolError`, `Unconfigured` degraded resolver | S1, S10 | worker |
| T3 | `src/youtube/search.rs`: the JSON-line parse over `miniserde`, tolerant of nulls | S2, S7 | worker |
| T4 | `src/youtube/resolve.rs`: the `-g` call, the format selector, and the `PathResolver` impl composed with `radio::stream::open` | S3, S4, S9 | worker |
| T5 | `Source::Youtube`, its row fact, the search screen, the status vocabulary | S1, S5 | worker |
| T6 | README: `## Requirements` and `## Privacy` name `yt-dlp`; the new section says what leaves the machine | S8 | inline |

## Log

**L1 — the author's request, verbatim:** "veamos como resolver el issue 70".

**L2 — the decision this logbook records.** Of the four ways to start, the author
chose *"Confirmar la excepción a #46 y diseñar"*: the criterion goes into issue
#70 first — **#46 refuses a sidecar, something without which velo cannot start or
work; `yt-dlp` is an optional process, detected with `which`, degrading to one
sentence, and velo runs identically without it** — and this document is the design
that follows, written before any code.

**L3 — the material facts behind the design, verified in the tree.** The deferred
seam exists and is reusable (`PathResolver::cached`/`open_streaming`,
`is_uncached_remote`, `start_as_pending`, `poll_pending_resolve`); no child-process
runner exists anywhere, `open_url` being fire-and-forget; the streaming decoder has
no Matroska/WebM demuxer, which is what picks `ext=m4a`; `miniserde` is the repo's
JSON crate by an existing decision (`Cargo.toml`); identity rides the
`subsonic://` pseudo-path precedent so `history.rs` and `saved.rs` keep their
formats; and `#46` closed as *none* with a rule that names a downloader, which is
why L2 exists.

**L4 — what each task landed, and where.** T1–T4 in `77f8e90`: the bounded
runner (`tool.rs`, six tests including the process-group kill and a grandchild
holding the pipe), the capability and the degraded resolver (`mod.rs`), the
miniserde search (`search.rs`), and `Entry::Youtube` with the resolver that
opens the ranged stream at play time (`resolve.rs`, `output.rs`, the twelve
`Entry` matches, S9's refusal). T5 in `0400db9`: the row, the screen
(`src/ui/youtube.rs`), and the resolver the binary composes now that two
sources own pseudo-paths. T6 in `d105b4d`. `scripts/check.sh` — fmt, clippy
`-D warnings`, test — green before each commit, 1279 tests.

**L5 — the defect slice D found, recorded because it was mine.** Slice C left
`is_uncached_remote` recognizing only `subsonic://` and remote URLs, so a
`youtube://` first entry never looked like it would wait: the start gates never
fired and the first resolve would have run on the event loop, breaking S6
without any test noticing. The pinning test
(`a_queued_youtube_entry_resolves_on_the_worker_never_the_event_loop`) is what
caught it. The lesson for a next source that owns a pseudo-path: the wait
question is a separate seam from the resolve one, and adding a scheme without
teaching it is a silent S6 regression.

**L6 — the verification found the seam's blind spot, and it was a blocker.** An
independent read of slices A–D found that `Entry::Youtube` plays and a
`youtube://` row rebuilt from a stored path does not: the history, a favourite,
a playlist and `velo --play youtube://<id>` all produce `Entry::File`, whose
arms asked `resolve` and got the resolver's honest refusal — a skipped track,
and no process ever spawned. B1 in the verification, and exactly the risk that
made this task's verification independent. Fixed at the seam rather than per
constructor: `open_first_entry` is the one place a start opens its first entry,
and a youtube pseudo-path asks the streaming question there and in the
producer's file arm, with the carried source still skipping a second resolve
(`tests/youtube_queued_rows.rs` pins both). F2 (a lengthless result refuses a
seek instead of re-resolving at an offset) and two smaller ones — `ctrl+s` on
such a row, and `next` under a station skipping a queued YouTube entry — came
with it.

**L7 — the native review, slice by slice, and the first one approved.** The
branch was re-partitioned into twelve commits of 23–649 authored lines each
(`d723900` … `0ef3846`), because the whole branch (4097 lines) was refused as a
candidate with `lens_context_budget_exceeded`. The first slice reviewed on its
own — the bounded child runner, `ad9cc0b`, over `d723900` — came back
**approved** (`review-88bbde3d97ac33ad`, lenses risk and resilience, three
advisory findings, none blocking): (R4-1) on the deadline path after the child
has already exited, the group kill targets a pid that may have been recycled,
an unrealistically narrow window; (R4-2) off unix, `end()` kills only the direct
child, so a grandchild holding the pipe leaves a drain thread blocked — unix is
correct and no unix target is affected; (R4-3) a mid-read pipe error is treated
as an empty successful read, so a caller could see success with truncated
output. All three are follow-ups for a later commit, not corrections to this
one. The remaining eleven commits each need the same per-slice review before
delivery; delivery itself — push, PR, merge, release — stays the author's.

**L8 — the first run against real `YouTube`, and what it cost.** Manual testing
against `yt-dlp 2026.10.26` found four things no fake binary could: (1) the
`--get-url` route printed a media URL that answered `403`, while the full
extraction (`--dump-single-json`) answers with URLs a plain ranged GET can use —
so the resolve now reads the extraction, not the URL print; (2) `YouTube` serves
AAC-LC (`mp4a.40.2`) and HE-AAC (`mp4a.40.5`) both as `m4a` and lists the
HE-AAC one first, and symphonia refuses HE-AAC at decode time ("aac too
complex"), so the picker chooses by codec, not container, and the test fixture
that had claimed AAC-LC inside a `webm` was corrected; (3) a live broadcast is an
HLS manifest, which this tree cannot demux — refused by name before any request,
and marked `✗` in the results; (4) the shared HTTP transport followed no
redirects, so a `302` to the host that serves the bytes read as a refusal.

**Still open, and it is a decision, not a bug: the extracted URL is a coin
flip.** Measured over six fresh extractions of one video, two produced a URL
that answered `206` and four a URL that refused every attempt (`403`); retrying
the same URL never helped, a new extraction sometimes did. So playback today
succeeds on roughly a third of attempts. Two ways out, and the cost is
different: ask the tool again on a refusal, up to a few attempts (keeps ranged
seeking, still fails when every attempt draws badly), or let the tool deliver
the bytes itself (`-o -` on stdout), which is reliable but makes a seek a fresh
spawn with `--download-sections` and retires the "resolved URL with a lifetime"
premise of S4.

**L10 — the future the author named: live broadcasts are a wanted feature.**
A live YouTube broadcast is refused today only because this tree has no HLS
demuxer — the refusal is a boundary, not a verdict. When HLS lands, the live
refusal (`LIVE_SENTENCE`, and `play_youtube`'s arm) comes out and the `✗` mark
in the results becomes the live vocabulary the station rows already teach;
nothing else in the design has to move: the entry, the identity, the queue and
the worker are already shaped for it.

**L9 — the architecture changed, because the measurement did.** Five
consecutive extractions of one video gave the same answer every time: the only
audio format whose URL answers a ranged GET is 139, which is HE-AAC and which
symphonia refuses with "aac too complex"; 140 (AAC-LC, the one that decodes),
251 (opus) and the rest answer `403` to a bare GET **and** to a GET carrying the
`http_headers` the extraction declares. So the refusal is not a coin flip and no
amount of re-extraction fixes it — my earlier "2 of 6 usable" reading was a bad
sample and is retracted here. What does work, first try, is letting the tool
fetch: `yt-dlp -x --audio-format mp3 -o - <url>` streams a decodable,
sequential MP3 to stdout. MP3 on purpose: symphonia decodes it without seeking a
container, which is the same shape the radio's ICY path already proves. The
price is `ffmpeg` for this row, a transcode, and a seek that becomes a fresh
spawn with `--download-sections` — a later slice. S4 above is rewritten to say
what is now true.
