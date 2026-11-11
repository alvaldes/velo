// The landing page's behaviour: the install picker, the copy buttons, the
// working player, and the hearth video's manners.
//
// Everything the page loads is in this folder; it asks nobody else for anything.

const REPO = 'alvaldes/velo';

const installs = {
  brew: {
    line: 'brew install alvaldes/velo/velo',
    hint: 'macOS and Linux. The tap follows each release within the hour, and brew upgrade velo moves you to the next one.',
  },
  script: {
    line: 'curl -fsSL https://alvaldes.github.io/velo/install.sh | sh',
    hint: 'The script picks the binary for this machine, checks it against the .sha256 CI published beside it, and puts velo in ~/.local/bin. No sudo.',
  },
  source: {
    line: `git clone https://github.com/${REPO} && cd velo && cargo install --path .`,
    hint: 'A clone and cargo: needs Rust 1.88 or newer, cmake or a system libopus, and the ALSA development headers on Linux.',
  },
};

const $ = (id) => document.getElementById(id);

// ---- install ----

function showInstall() {
  const checked = document.querySelector('input[name="platform"]:checked');
  const install = installs[checked?.value] ?? installs.brew;
  $('line').textContent = install.line;
  $('hint').textContent = install.hint;
}

for (const radio of document.querySelectorAll('input[name="platform"]')) {
  radio.addEventListener('change', showInstall);
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

showInstall();

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
