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

/**
 * The largest whole number of device pixels per cell that still fits the screen.
 *
 * The code has to arrive at the camera as pixels, not as a resampled
 * approximation of pixels, so the cell size is chosen to divide the display
 * exactly rather than to fill it exactly. Filling the last few percent by
 * scaling would undo the point.
 */
function fitCellSize(grid) {
  const quiet = 4;
  const cells = grid + 2 * quiet;
  const ratio = window.devicePixelRatio || 1;
  const shortest = Math.min(window.screen.width, window.screen.height) * ratio;
  return Math.max(3, Math.floor(shortest / cells));
}

function describeProfile(profile, cellPx) {
  const side = (profile.grid + 8) * cellPx;
  return `${profile.grid}×${profile.grid} cells at ${profile.bitsPerCell} bits, ` +
    `${profile.payloadCapacity} bytes per frame. ` +
    `At ${cellPx} device pixels per cell the code is ${side}×${side}.`;
}

function refreshProfileNote() {
  const profile = PROFILES[ui.profile.selectedIndex];
  if (!profile) return;
  ui.profileNote.textContent = describeProfile(profile, fitCellSize(profile.grid));
}

/** Starts painting. */
async function start() {
  const file = ui.file.files?.[0];
  if (!file) return;

  const profile = PROFILES[ui.profile.selectedIndex];
  const cellPx = fitCellSize(profile.grid);

  let emitter;
  try {
    const buffer = new Uint8Array(await file.arrayBuffer());
    // Only has to be unlikely to collide with another transfer being filmed
    // nearby; it is not a secret and the protocol does not treat it as one.
    const session = crypto.getRandomValues(new Uint32Array(1))[0];
    emitter = new Emitter(file.name, buffer, profile.id, cellPx, session);
  } catch (error) {
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

  ui.stage.classList.add('showing');
  await enterFullscreen();
  await keepAwake();

  running = {
    emitter,
    context,
    side,
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

  running.frames += 1;
  const pass = Math.floor(running.frames / Math.max(1, running.perPass)) + 1;
  ui.stageStatus.textContent = `frame ${running.frames} · pass ${pass} · keep recording`;
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

ui.hold.addEventListener('input', () => {
  ui.holdValue.textContent = ui.hold.value;
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

try {
  await init();
  PROFILES = JSON.parse(profiles());

  for (const profile of PROFILES) {
    const option = document.createElement('option');
    option.textContent = profile.name;
    ui.profile.append(option);
  }
  // P2-standard is the default the specification names.
  ui.profile.selectedIndex = Math.min(1, PROFILES.length - 1);
  refreshProfileNote();
} catch (error) {
  say(`The protocol module failed to load: ${error}`, 'bad');
  ui.start.disabled = true;
}
