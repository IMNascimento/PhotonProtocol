// The sending half. Picks a file, hands it to the protocol, and paints the
// frames the protocol produces.
//
// The one thing this file must get right is that a code pixel reaches the
// display as a device pixel. Everything else here is a form.

import init, { Emitter, profiles } from '../photon/photon_wasm.js';

const ui = {
  file: document.getElementById('file'),
  profile: document.getElementById('profile'),
  profileNote: document.getElementById('profile-note'),
  hold: document.getElementById('hold'),
  holdValue: document.getElementById('hold-value'),
  start: document.getElementById('start'),
  stop: document.getElementById('stop'),
  summary: document.getElementById('summary'),
  status: document.getElementById('status'),
  stage: document.getElementById('stage'),
  stageStatus: document.getElementById('stage-status'),
  canvas: document.getElementById('frame'),
  facts: {
    name: document.getElementById('fact-name'),
    size: document.getElementById('fact-size'),
    compression: document.getElementById('fact-compression'),
    frame: document.getElementById('fact-frame'),
    pass: document.getElementById('fact-pass'),
  },
};

/**
 * Whether to send painted frames back to the server that served this page.
 *
 * Off unless the address says `?capture`. What is being drawn is the other half
 * of any question about what is being read, and until both halves are on disk
 * together the answer is guesswork.
 */
const CAPTURING = new URLSearchParams(location.search).has('capture');

/** How many painted frames to keep. One pass is plenty. */
const CAPTURE_LIMIT = 12;

/** Profile descriptions, as the protocol reports them. */
let PROFILES = [];

/** The running emission, or null. */
let running = null;

function say(message, kind = '') {
  ui.status.textContent = message;
  ui.status.className = `status ${kind}`;
  ui.status.classList.remove('hidden');
}

function bytes(count) {
  if (count < 1024) return `${count} bytes`;
  if (count < 1024 * 1024) return `${(count / 1024).toFixed(1)} KB`;
  return `${(count / 1024 / 1024).toFixed(2)} MB`;
}

/** Cells of white margin the format puts around the code area. */
const QUIET_ZONE_CELLS = 4;

/**
 * Fraction of the available box to fill.
 *
 * The last couple of percent is not worth having. Browser chrome appears and
 * disappears, fullscreen settles a frame late, and rounding goes whichever way
 * it goes — and if any of that clips an edge, it clips a corner pattern, and a
 * code with a missing corner is not a code at all. It fails invisibly, too: the
 * screen still looks right to a person holding a camera at it.
 */
const FIT_MARGIN = 0.96;

/**
 * The largest whole number of device pixels per cell that fits a given box.
 *
 * `box` is measured in CSS pixels from the element the code will actually be
 * drawn in — not from `window.screen`, which on a desktop is the monitor rather
 * than the window, and produced a code larger than the space available for it.
 *
 * Whole pixels per cell, because the code has to reach the camera as pixels
 * rather than as a resampled approximation of pixels. Filling the remaining
 * fraction by scaling would undo the point of drawing it carefully.
 */
function fitCellSize(grid, box) {
  const cells = grid + 2 * QUIET_ZONE_CELLS;
  const ratio = window.devicePixelRatio || 1;
  const shortest = Math.min(box.width, box.height) * ratio * FIT_MARGIN;
  return Math.max(3, Math.floor(shortest / cells));
}

/**
 * Whether a profile can be drawn on this screen at a size a camera can read.
 *
 * `minCellPx` comes from the protocol rather than from here. Each cell carries
 * a 4x4 shape mask, and a cell too small to give every sub-cell pixels of its
 * own loses the shape — which is most of the payload: 3 of the 5 bits in
 * P2-standard. The loss is not gradual for the 8-shape profiles. Painted with
 * no camera in the path at all, P2-standard reads 5.9% of its cells wrongly at
 * 7 pixels per cell and 0.0% at 8, and in every case the colour comes through
 * perfectly and the shape is what fails.
 */
function fitsOnScreen(profile, box) {
  return fitCellSize(profile.grid, box) >= profile.minCellPx;
}

/**
 * The densest profile this screen can actually draw, or the sparsest if none
 * can.
 *
 * Density is what everyone wants and the screen is what decides whether they
 * may have it. Choosing for them beats defaulting to P2-standard everywhere and
 * letting a laptop paint cells too small to read, which looks perfectly fine to
 * whoever is pointing a camera at it.
 */
function bestProfile(box) {
  for (let index = PROFILES.length - 1; index >= 0; index -= 1) {
    if (fitsOnScreen(PROFILES[index], box)) return index;
  }
  return 0;
}

/** The box the code will be drawn into, in CSS pixels. */
function stageBox() {
  const rect = ui.stage.getBoundingClientRect();
  if (rect.width > 1 && rect.height > 1) {
    return { width: rect.width, height: rect.height };
  }
  // The stage is still hidden, so fall back to the viewport. Also the right
  // answer for the estimate shown before anything starts.
  return { width: window.innerWidth, height: window.innerHeight };
}

/** Waits for layout to settle after a fullscreen change. */
function nextFrame() {
  return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
}

function describeProfile(profile, cellPx) {
  const side = (profile.grid + 2 * QUIET_ZONE_CELLS) * cellPx;
  const facts =
    `${profile.grid}×${profile.grid} cells at ${profile.bitsPerCell} bits, ` +
    `${profile.payloadCapacity} bytes per frame. On this display that is ` +
    `${cellPx} pixels per cell and a code of ${side}×${side} pixels.`;

  if (cellPx >= profile.minCellPx) return facts;

  const alternative = PROFILES.find((other) => fitsOnScreen(other, stageBox()));
  return `${facts} That is below the ${profile.minCellPx} pixels a cell needs for its ` +
    'shape to survive a camera, so this will not decode on this screen. ' +
    (alternative
      ? `Choose ${alternative.name}, which fits.`
      : 'No profile fits this screen; send from a larger display.');
}

/** Relabels the profiles this screen cannot draw legibly. */
function refreshProfileOptions() {
  const box = stageBox();
  PROFILES.forEach((profile, index) => {
    const option = ui.profile.options[index];
    if (!option) return;
    option.textContent = fitsOnScreen(profile, box)
      ? profile.name
      : `${profile.name} — too dense for this screen`;
  });
}

function refreshProfileNote() {
  const profile = PROFILES[ui.profile.selectedIndex];
  if (!profile) return;
  const cellPx = fitCellSize(profile.grid, stageBox());
  ui.profileNote.textContent = describeProfile(profile, cellPx);
  ui.profileNote.classList.toggle('bad', cellPx < profile.minCellPx);
}

/** Starts painting. */
async function start() {
  const file = ui.file.files?.[0];
  if (!file) return;

  const profile = PROFILES[ui.profile.selectedIndex];

  // Show the stage and go fullscreen *before* measuring. The box the code has
  // to fit is the one it will actually be drawn in, and that box is not known
  // until the browser has finished changing its mind about window chrome.
  ui.stage.classList.add('showing');
  await enterFullscreen();
  await nextFrame();

  const box = stageBox();
  const cellPx = fitCellSize(profile.grid, box);

  // Refuse rather than paint something unreadable. A code drawn below the floor
  // looks entirely normal on the screen and to the person filming it; the only
  // sign is a receiver that never finishes, which is a long way from the cause.
  // If a sparser profile fits, this is the user's choice to correct; if none
  // does, the screen is simply too small and no choice here changes that.
  if (cellPx < profile.minCellPx) {
    const alternative = PROFILES.find((other) => fitsOnScreen(other, box));
    ui.stage.classList.remove('showing');
    await exitFullscreen();
    say(
      alternative
        ? `${profile.name} draws ${cellPx}-pixel cells on this screen and a camera ` +
          `cannot read the shapes below ${profile.minCellPx}. Choose ${alternative.name} and start again.`
        : `This screen draws ${cellPx}-pixel cells even at ${PROFILES[0].name}, and a ` +
          `camera cannot read the shapes below ${PROFILES[0].minCellPx}. Send from a larger display.`,
      'bad',
    );
    return;
  }

  let emitter;
  try {
    const buffer = new Uint8Array(await file.arrayBuffer());
    // Only has to be unlikely to collide with another transfer being filmed
    // nearby; it is not a secret and the protocol does not treat it as one.
    const session = crypto.getRandomValues(new Uint32Array(1))[0];
    emitter = new Emitter(file.name, buffer, profile.id, cellPx, session);
  } catch (error) {
    ui.stage.classList.remove('showing');
    say(`This file cannot be sent: ${error}`, 'bad');
    return;
  }

  const manifest = JSON.parse(emitter.manifest());
  const side = emitter.side();

  ui.facts.name.textContent = manifest.name;
  ui.facts.size.textContent = bytes(manifest.originalSize);
  ui.facts.compression.textContent = manifest.compression;
  ui.facts.frame.textContent = `${side}×${side} px, ${cellPx} px per cell`;
  ui.facts.pass.textContent = `${emitter.framesPerPass()} frames`;
  ui.summary.classList.remove('hidden');

  ui.canvas.width = side;
  ui.canvas.height = side;
  // Lay the canvas out so one canvas pixel lands on one device pixel.
  const cssSide = side / (window.devicePixelRatio || 1);
  ui.canvas.style.width = `${cssSide}px`;
  ui.canvas.style.height = `${cssSide}px`;

  const context = ui.canvas.getContext('2d', { alpha: false, willReadFrequently: false });
  context.imageSmoothingEnabled = false;

  // If this ever fails the code is being clipped, which removes the corner
  // patterns and makes the frame unreadable while still looking fine.
  if (cssSide > box.width + 1 || cssSide > box.height + 1) {
    stop();
    say(
      `The code needs ${Math.ceil(cssSide)} pixels and only ${Math.floor(
        Math.min(box.width, box.height),
      )} are available. Use a larger window, or a more conservative profile.`,
      'bad',
    );
    return;
  }

  await keepAwake();

  running = {
    emitter,
    context,
    side,
    cellPx,
    hold: Number(ui.hold.value),
    ticks: 0,
    frames: 0,
    perPass: emitter.framesPerPass(),
    raf: 0,
  };
  running.raf = requestAnimationFrame(paint);
}

/**
 * Paints one frame per `hold` display refreshes.
 *
 * Tying emission to the refresh rate rather than to a timer is what keeps a
 * code on screen for whole refreshes. A frame swapped mid-refresh reaches the
 * camera as two half codes, and the receiver can only throw those away.
 */
function paint() {
  if (!running) return;
  running.raf = requestAnimationFrame(paint);

  if (running.ticks++ % running.hold !== 0) return;

  let rgba;
  try {
    rgba = running.emitter.nextFrame();
  } catch (error) {
    stop();
    say(`Emission stopped: ${error}`, 'bad');
    return;
  }

  const picture = new ImageData(new Uint8ClampedArray(rgba), running.side, running.side);
  running.context.putImageData(picture, 0, 0);

  if (CAPTURING && running.frames < CAPTURE_LIMIT) {
    sendPainted(running);
  }

  running.frames += 1;
  const pass = Math.floor(running.frames / Math.max(1, running.perPass)) + 1;
  ui.stageStatus.textContent =
    `${running.side}px · ${running.cellPx}px per cell · frame ${running.frames} · pass ${pass}`;
}

/** Sends one painted frame to the development server. */
function sendPainted(state) {
  const query = new URLSearchParams({
    kind: 'sent',
    outcome: `frame${String(state.frames).padStart(3, '0')}`,
    report: JSON.stringify({ side: state.side, cellPx: state.cellPx, frame: state.frames }),
  });

  ui.canvas.toBlob((blob) => {
    if (!blob) return;
    fetch(`/capture?${query}`, { method: 'POST', body: blob }).catch(() => {
      // Nothing about the emission depends on this landing.
    });
  }, 'image/png');
}

function stop() {
  if (running) {
    cancelAnimationFrame(running.raf);
    running = null;
  }
  ui.stage.classList.remove('showing');
  releaseWake();
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
}

/** Leaves fullscreen, so that a message written on the page can be read. */
async function exitFullscreen() {
  if (!document.fullscreenElement) return;
  try {
    await document.exitFullscreen();
  } catch {
    // Nothing to do about it, and the message is still on the page underneath.
  }
}

async function enterFullscreen() {
  try {
    await ui.stage.requestFullscreen({ navigationUI: 'hide' });
  } catch {
    // Fullscreen is a nicety. Some browsers refuse it outside a direct
    // gesture and some refuse it entirely; the codes are just as readable in
    // a window, so this is never worth failing over.
  }
}

let wakeLock = null;

async function keepAwake() {
  try {
    wakeLock = await navigator.wakeLock?.request('screen');
  } catch {
    // A screen that sleeps halfway through costs the viewer some frames, and
    // the fountain code absorbs exactly that. Not worth an error.
  }
}

function releaseWake() {
  wakeLock?.release?.().catch(() => {});
  wakeLock = null;
}

/** Says what the hold setting means in codes per second. */
function refreshHoldNote() {
  const note = document.getElementById('hold-note');
  if (!note) return;
  const hold = Number(ui.hold.value);
  // Most displays are 60 Hz; the exact figure only shifts the estimate.
  const rate = 60 / hold;
  note.textContent =
    `About ${rate.toFixed(0)} codes per second. A camera recording at 30 frames ` +
    `per second needs the codes to change slower than that to catch whole ones.`;
}

if (CAPTURING) {
  const banner = document.createElement('div');
  banner.className = 'status bad';
  banner.textContent =
    `Diagnostic capture is ON. The first ${CAPTURE_LIMIT} painted frames will be sent to ` +
    `${location.origin}, which is the server that served this page. Remove ` +
    '"?capture" from the address to turn it off.';
  document.body.prepend(banner);
}

ui.hold.addEventListener('input', () => {
  ui.holdValue.textContent = ui.hold.value;
  refreshHoldNote();
  if (running) running.hold = Number(ui.hold.value);
});

ui.profile.addEventListener('change', refreshProfileNote);
ui.file.addEventListener('change', () => {
  ui.start.disabled = !ui.file.files?.length;
});
ui.start.addEventListener('click', start);
ui.stop.addEventListener('click', stop);

document.addEventListener('fullscreenchange', () => {
  if (!document.fullscreenElement && running) stop();
});

// Which profiles fit is a property of the window, and the window changes: a
// laptop that could not draw P2-standard in half a screen can in a whole one,
// and a rotated phone changes its shortest side entirely.
window.addEventListener('resize', () => {
  if (running || !PROFILES.length) return;
  refreshProfileOptions();
  refreshProfileNote();
});

try {
  await init();
  PROFILES = JSON.parse(profiles());

  for (let index = 0; index < PROFILES.length; index += 1) {
    ui.profile.append(document.createElement('option'));
  }
  refreshProfileOptions();
  // The densest profile this screen can draw legibly, rather than the one the
  // specification names as standard. P2-standard is the right default for a
  // screen that can carry it and is silently unreadable on one that cannot.
  ui.profile.selectedIndex = bestProfile(stageBox());
  refreshProfileNote();
  refreshHoldNote();
} catch (error) {
  say(`The protocol module failed to load: ${error}`, 'bad');
  ui.start.disabled = true;
}
