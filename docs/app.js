// The landing page's behaviour: the install picker, the copy buttons, the
// working player, and the hearth video's manners.
//
// Everything the page loads is in this folder. The one request that leaves it
// is the one velo itself makes: GitHub's latest-release answer, which turns the
// download button into a direct link to the archive for the chosen platform.
// When it fails the button still goes to the Releases page.

const REPO = 'alvaldes/velo';
const INSTALL = `curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sh`;
const SOURCE = `git clone https://github.com/${REPO} && cd velo && cargo install --path .`;
const SCRIPT_HINT =
  'The script picks the binary for this machine, checks it against the .sha256 CI published beside it, and puts velo in ~/.local/bin. No sudo.';

const platforms = {
  'aarch64-apple-darwin': { name: 'macOS (Apple silicon)', line: INSTALL, hint: SCRIPT_HINT },
  'x86_64-apple-darwin': { name: 'macOS (Intel)', line: INSTALL, hint: SCRIPT_HINT },
  'x86_64-unknown-linux-gnu': { name: 'Linux (x64)', line: INSTALL, hint: SCRIPT_HINT },
  'aarch64-unknown-linux-gnu': { name: 'Linux (arm64)', line: INSTALL, hint: SCRIPT_HINT },
  source: {
    name: 'source',
    line: SOURCE,
    hint: 'A clone and cargo: needs Rust 1.88 or newer, cmake or a system libopus, and the ALSA development headers on Linux.',
  },
};

const $ = (id) => document.getElementById(id);

// ---- install ----

const line = $('line');
const hint = $('hint');
const download = $('download');
const releaseNote = $('release');

// Filled in by the latest-release answer: the tag, and each archive by target.
let release = null;

function megabytes(bytes) {
  return `${(bytes / 1_048_576).toFixed(1)} MB`;
}

function chosen() {
  const checked = document.querySelector('input[name="platform"]:checked');
  return platforms[checked?.value] ? checked.value : 'aarch64-apple-darwin';
}

function showPlatform() {
  const key = chosen();
  const platform = platforms[key];
  line.textContent = platform.line;
  hint.textContent = platform.hint;

  if (key === 'source') {
    download.textContent = 'Download the source';
    download.href = release
      ? `https://github.com/${REPO}/archive/refs/tags/${release.tag}.tar.gz`
      : `https://github.com/${REPO}`;
    releaseNote.textContent = release ? `${release.tag}, .tar.gz` : 'from GitHub';
    return;
  }

  const asset = release?.assets.get(key);
  download.textContent = `Download for ${platform.name}`;
  download.href = asset ? asset.url : `https://github.com/${REPO}/releases/latest`;
  releaseNote.textContent = asset
    ? `${release.tag}, ${megabytes(asset.size)} .tar.gz`
    : '.tar.gz from the latest release';
}

// A guess, never a lock: the picker stays the reader's. An Apple silicon Mac
// reports itself as Intel, so macOS keeps the arm64 default.
function guessPlatform() {
  const agent = navigator.userAgent;
  let key = null;
  if (/Linux/.test(agent) && !/Android/.test(agent)) {
    key = /aarch64|arm64/.test(agent) ? 'aarch64-unknown-linux-gnu' : 'x86_64-unknown-linux-gnu';
  }
  const radio = key && document.querySelector(`input[name="platform"][value="${key}"]`);
  if (radio) radio.checked = true;
}

async function findRelease() {
  try {
    const answer = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers: { Accept: 'application/vnd.github+json' },
    });
    if (!answer.ok) return;
    const body = await answer.json();
    const assets = new Map();
    for (const asset of body.assets ?? []) {
      const target = Object.keys(platforms).find((key) => asset.name === `velo-${body.tag_name}-${key}.tar.gz`);
      if (target) assets.set(target, { url: asset.browser_download_url, size: asset.size });
    }
    release = { tag: body.tag_name, assets };
    showPlatform();
  } catch {
    // Offline or refused: the button keeps pointing at the Releases page.
  }
}

for (const radio of document.querySelectorAll('input[name="platform"]')) {
  radio.addEventListener('change', showPlatform);
}

for (const button of document.querySelectorAll('.copy')) {
  const label = button.textContent;
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText($(button.dataset.copyFrom).textContent);
      button.textContent = 'copied';
    } catch {
      button.textContent = 'select it';
    }
    setTimeout(() => { button.textContent = label; }, 1600);
  });
}

guessPlatform();
showPlatform();
findRelease();

// ---- the player ----

const player = $('player');
const track = $('track');
const toggle = $('toggle');
const seek = $('seek');
const volume = $('volume');
const volumeValue = $('volume-value');
const clock = $('clock');
const state = $('state');
const marker = $('marker');
const more = $('more');
const bars = [...document.querySelectorAll('#spectrum i')];

// The keys velo answers to that this demo does not: each one opens the card.
const ELSEWHERE = new Set([
  'j', 'k', 'g', 'G', '/', '1', '2', '3', '4', ':', '>', '.', 'n', '<', ',', 'z', 'r', 'm',
  'P', 'o', 'L', 'I', 'u', 'd', 'p', 'a', 'A', 'x', 'J', 'K', 'S', 'e', 'v', 'V', '?', 'R',
  'f', 'F', 'N', 'i', 'q', 'ArrowUp', 'ArrowDown',
]);

let audio = null; // { context, analyser, gain, bins } once the reader has pressed play
let frame = 0;
let seeking = false;

function time(seconds) {
  const whole = Math.max(0, Math.floor(seconds));
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`;
}

function fill(input) {
  const span = input.max - input.min;
  input.style.setProperty('--p', `${span ? ((input.value - input.min) / span) * 100 : 0}%`);
}

// The spectrum needs the samples, so the track goes through an analyser; the
// volume is a gain after it, so turning it down does not flatten the bars.
function connect() {
  if (audio || !(window.AudioContext || window.webkitAudioContext)) return;
  const context = new (window.AudioContext || window.webkitAudioContext)();
  const analyser = context.createAnalyser();
  analyser.fftSize = 2048;
  analyser.smoothingTimeConstant = 0.78;
  const gain = context.createGain();
  context.createMediaElementSource(track).connect(analyser).connect(gain).connect(context.destination);
  audio = { context, analyser, gain, bins: new Uint8Array(analyser.frequencyBinCount) };
  setVolume(Number(volume.value));
}

function setVolume(value) {
  const level = Math.min(100, Math.max(0, Math.round(value)));
  volume.value = level;
  volumeValue.textContent = level;
  fill(volume);
  if (audio) audio.gain.gain.value = level / 100;
  else track.volume = level / 100;
}

function showPosition(seconds) {
  clock.textContent = time(seconds);
  seek.value = seconds;
  fill(seek);
}

function seekTo(seconds) {
  const length = Number.isFinite(track.duration) ? track.duration : Number(seek.max);
  const position = Math.min(length, Math.max(0, seconds));
  track.currentTime = position;
  showPosition(position);
  showState();
}

// One bar per band, the bands spaced by ratio from 50 Hz to 12 kHz the way an
// ear hears them, each showing the loudest bin inside it.
function drawSpectrum() {
  const { analyser, context, bins } = audio;
  analyser.getByteFrequencyData(bins);
  const perBin = context.sampleRate / analyser.fftSize;
  let lively = false;
  bars.forEach((bar, index) => {
    const low = 50 * (240 ** (index / bars.length));
    const high = 50 * (240 ** ((index + 1) / bars.length));
    const from = Math.floor(low / perBin);
    const to = Math.max(from + 1, Math.ceil(high / perBin));
    let peak = 0;
    for (let bin = from; bin < to && bin < bins.length; bin++) peak = Math.max(peak, bins[bin]);
    const height = Math.max(2, (peak / 255) ** 1.6 * 100);
    if (height > 2.5) lively = true;
    bar.style.height = `${height}%`;
  });
  // Keep drawing through the fall after a pause, then stop asking for frames.
  frame = track.paused && !lively ? 0 : requestAnimationFrame(drawSpectrum);
}

function showState() {
  const playing = !track.paused;
  const stopped = !playing && track.currentTime === 0;
  toggle.textContent = playing ? '❚❚ pause' : '▶ play';
  state.textContent = playing ? '▶ playing' : stopped ? '■ stopped' : '⏸ paused';
  marker.textContent = playing ? '▶' : stopped ? '' : '⏸';
  player.classList.toggle('is-playing', playing);
  if (playing && audio && !frame) frame = requestAnimationFrame(drawSpectrum);
}

async function play() {
  connect();
  player.classList.add('is-live');
  try {
    if (audio?.context.state === 'suspended') await audio.context.resume();
    await track.play();
  } catch {
    state.textContent = '■ could not play here';
  }
}

function togglePlay() {
  if (track.paused) play();
  else track.pause();
}

function showMore() {
  more.hidden = false;
  more.querySelector('a').focus();
}

function hideMore() {
  if (more.hidden) return;
  more.hidden = true;
  player.focus();
}

toggle.addEventListener('click', togglePlay);
$('row').addEventListener('click', togglePlay);
$('more-close').addEventListener('click', hideMore);
for (const link of more.querySelectorAll('a')) link.addEventListener('click', () => { more.hidden = true; });
for (const control of player.querySelectorAll('[data-more]')) control.addEventListener('click', showMore);

track.addEventListener('play', showState);
track.addEventListener('pause', showState);
track.addEventListener('loadedmetadata', () => {
  seek.max = Math.floor(track.duration);
  $('length').textContent = time(track.duration);
  fill(seek);
});
track.addEventListener('timeupdate', () => { if (!seeking) showPosition(track.currentTime); });
track.addEventListener('ended', () => {
  track.currentTime = 0;
  showPosition(0);
  showState();
});

seek.addEventListener('input', () => {
  seeking = true;
  clock.textContent = time(Number(seek.value));
  fill(seek);
});
seek.addEventListener('change', () => {
  seeking = false;
  seekTo(Number(seek.value));
});
volume.addEventListener('input', () => setVolume(Number(volume.value)));

// The interface's own keys, while the player has the focus. A key pressed on
// one of its controls is that control's first: space on a button presses it,
// an arrow on a slider moves it.
player.addEventListener('keydown', (event) => {
  if (event.metaKey || event.ctrlKey || event.altKey) return;

  if (event.key === 'Escape') {
    hideMore();
    return;
  }
  if (!more.hidden) return;

  const onControl = event.target instanceof HTMLButtonElement || event.target instanceof HTMLAnchorElement;
  const onSlider = event.target instanceof HTMLInputElement;
  let handled = true;

  if (event.key === ' ' || event.key === 'Enter') {
    if (onControl) return;
    togglePlay();
  } else if (event.key.startsWith('Arrow') && onSlider) {
    return;
  } else if (event.key === 'ArrowLeft') seekTo(track.currentTime - (event.shiftKey ? 30 : 5));
  else if (event.key === 'ArrowRight') seekTo(track.currentTime + (event.shiftKey ? 30 : 5));
  else if (event.key === '+' || event.key === '=') setVolume(Number(volume.value) + 5);
  else if (event.key === '-') setVolume(Number(volume.value) - 5);
  else if (event.key === ']') setVolume(Number(volume.value) + 1);
  else if (event.key === '[') setVolume(Number(volume.value) - 1);
  else if (ELSEWHERE.has(event.key)) showMore();
  else handled = false;

  if (handled) event.preventDefault();
});

fill(seek);
setVolume(Number(volume.value));
showState();

// ---- the hearth ----

// The video loops only for readers who have not asked for less motion; for
// the rest the poster is the picture.
const hearth = $('hearth');
const calm = window.matchMedia('(prefers-reduced-motion: reduce)');

function settleHearth() {
  if (calm.matches) hearth.pause();
  else hearth.play().catch(() => {});
}

calm.addEventListener('change', settleHearth);
settleHearth();
