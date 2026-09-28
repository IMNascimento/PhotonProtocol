// The sending half. Picks a file, hands it to the protocol, and paints the
// frames the protocol produces.
//
// Two things this file must get right. A code pixel has to reach the display
// as a device pixel, and a code has to stay on the display long enough for a
// camera to take a whole picture of it. Everything else here is a form.

import init, { Emitter, profiles } from '../photon/photon_wasm.js';
import { translator, translatePage } from '../shared/i18n.js';

const t = translator({
  title: { en: 'PhotonProtocol — send', pt: 'PhotonProtocol — enviar' },
  heading: { en: 'Send a file', pt: 'Enviar um arquivo' },
  tagline: {
    en: "This screen paints the file. Point the other device's camera at it.",
    pt: 'Esta tela desenha o arquivo. Aponte a câmera do outro aparelho para ela.',
  },
  fileLegend: { en: 'File', pt: 'Arquivo' },
  fileLabel: {
    en: 'A photo, a document — anything up to a few megabytes',
    pt: 'Uma foto, um documento — qualquer coisa de até alguns megabytes',
  },
  start: { en: 'Start sending', pt: 'Começar a enviar' },
  stop: { en: 'Stop', pt: 'Parar' },
  detailsSummary: { en: 'Settings', pt: 'Ajustes' },
  rateLabel: { en: 'Codes per second', pt: 'Códigos por segundo' },
  profileLabel: { en: 'Density', pt: 'Densidade' },
  factName: { en: 'Name', pt: 'Nome' },
  factSize: { en: 'Size', pt: 'Tamanho' },
  factCompression: { en: 'Compression', pt: 'Compressão' },
  factFrame: { en: 'Code', pt: 'Código' },
  factPass: { en: 'One full pass', pt: 'Uma passada completa' },
  tipsHeading: { en: 'For a quick transfer', pt: 'Para transferir rápido' },
  tipBright: {
    en: 'Screen brightness to maximum; turn off night light and any colour filter.',
    pt: 'Brilho da tela no máximo; desligue a luz noturna e qualquer filtro de cor.',
  },
  tipZoom: {
    en: "Leave the browser's zoom at 100%.",
    pt: 'Deixe o zoom do navegador em 100%.',
  },
  tipGlare: {
    en: 'Keep lamps and windows from reflecting on the screen.',
    pt: 'Evite reflexo de lâmpada ou janela na tela.',
  },
  tipForever: {
    en: 'It sends until you stop it. The receiver says when it has the whole file.',
    pt: 'O envio continua até você parar. É o receptor que avisa quando tem o arquivo inteiro.',
  },
  back: { en: 'Back', pt: 'Voltar' },
  receiveInstead: { en: 'Receive a file instead', pt: 'Receber um arquivo' },

  estimate: {
    en: '{size}. At {rate} codes a second that is about {seconds} s for a receiver that catches every code.',
    pt: '{size}. A {rate} códigos por segundo, leva uns {seconds} s para um receptor que pegue todos os códigos.',
  },
  rateNote: {
    en: 'Each code stays up for {ms} ms. A phone takes a picture every 33 ms and needs the code to hold still for a whole one; 10 a second suits most phones, and a fast one keeps up with 15.',
    pt: 'Cada código fica {ms} ms na tela. Um celular tira uma imagem a cada 33 ms e precisa que o código fique parado durante uma inteira; 10 por segundo serve para a maioria, e um celular rápido acompanha 15.',
  },
  profileFacts: {
    en: '{grid}×{grid} cells, {bytes} bytes a code, drawn here at {cellPx} pixels a cell.',
    pt: '{grid}×{grid} células, {bytes} bytes por código, desenhado aqui com {cellPx} pixels por célula.',
  },
  profileNeeds: {
    en: ' A camera has to see about {perCell} of its own pixels for each cell, so the code must be at least {needed} pixels wide in its picture: {verdict}',
    pt: ' A câmera precisa enxergar uns {perCell} pixels dela por célula, então o código tem de ocupar pelo menos {needed} pixels de largura na imagem: {verdict}',
  },
  verdictAny: {
    en: 'any phone camera manages that.',
    pt: 'qualquer câmera de celular consegue.',
  },
  verdictFullHd: {
    en: 'a Full HD camera manages that from close up.',
    pt: 'uma câmera Full HD consegue, de perto.',
  },
  verdict4k: {
    en: 'that takes a 4K camera. Set the receiver to 4K.',
    pt: 'isso exige câmera 4K. Ajuste o receptor para 4K.',
  },
  profileTooSmall: {
    en: ' This window is too small to draw it: it would get {cellPx}-pixel cells and needs {minimum}.',
    pt: ' Esta janela é pequena demais para desenhá-lo: as células teriam {cellPx} pixels e precisam de {minimum}.',
  },
  optionRecommended: { en: '{name} — recommended', pt: '{name} — recomendado' },
  optionTooDense: { en: '{name} — does not fit this window', pt: '{name} — não cabe nesta janela' },
  refuseProfile: {
    en: '{name} would be drawn with {cellPx}-pixel cells in this window and cannot be read below {minimum}. Choose {alternative}, or make the window larger.',
    pt: '{name} seria desenhado com células de {cellPx} pixels nesta janela e não dá para ler abaixo de {minimum}. Escolha {alternative} ou aumente a janela.',
  },
  refuseScreen: {
    en: 'This window is too small to draw any code a camera could read. Make it larger, or use a larger screen.',
    pt: 'Esta janela é pequena demais para desenhar um código legível. Aumente a janela ou use uma tela maior.',
  },
  cannotSend: { en: 'This file cannot be sent: {error}', pt: 'Este arquivo não pode ser enviado: {error}' },
  stopped: { en: 'Sending stopped: {error}', pt: 'O envio parou: {error}' },
  clipped: {
    en: 'The code needs {needed} pixels and only {available} are available. Use a larger window.',
    pt: 'O código precisa de {needed} pixels e só há {available}. Use uma janela maior.',
  },
  frameFact: { en: '{side}×{side} px, {cellPx} px a cell', pt: '{side}×{side} px, {cellPx} px por célula' },
  passFact: { en: '{frames} codes, {seconds} s', pt: '{frames} códigos, {seconds} s' },
  stage: {
    en: '{name} · code {frame} · pass {pass} · {rate}/s',
    pt: '{name} · código {frame} · passada {pass} · {rate}/s',
  },
  moduleFailed: {
    en: 'The protocol module failed to load: {error}',
    pt: 'O módulo do protocolo não carregou: {error}',
  },
  captureBanner: {
    en: 'Diagnostic capture is ON. The first {limit} codes painted, and this page\'s measurements, are sent to {origin}, the server this page came from. Remove "?capture" from the address to turn it off.',
    pt: 'Captura de diagnóstico LIGADA. Os primeiros {limit} códigos desenhados, e as medições desta página, são enviados para {origin}, o servidor de onde esta página veio. Tire "?capture" do endereço para desligar.',
  },
});

const ui = {
  file: document.getElementById('file'),
  estimate: document.getElementById('estimate'),
  profile: document.getElementById('profile'),
  profileNote: document.getElementById('profile-note'),
  rate: document.getElementById('rate'),
  rateValue: document.getElementById('rate-value'),
  rateNote: document.getElementById('rate-note'),
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

/** How often measurements are sent while capturing. */
const LOG_INTERVAL_MS = 2000;

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
  if (count < 1024) return `${count} B`;
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
 * Camera pixels a cell has to cover to be read, by how many shapes the profile
 * draws.
 *
 * Measured with `photon film` rather than reasoned about. Four shapes are four
 * half-cells, whose finest feature is half a cell wide; eight shapes use
 * quarter-cells. A camera needs a little over two of its pixels to resolve a
 * feature, and the figures below leave it some room.
 */
function cameraPixelsPerCell(profile) {
  return profile.shapes > 4 ? 8 : 6;
}

/**
 * The largest whole number of device pixels per cell that fits a given box.
 *
 * `box` is measured in CSS pixels from the element the code will actually be
 * drawn in — not from `window.screen`, which on a desktop is the monitor rather
 * than the window, and produced a code larger than the space available for it.
 *
 * Whole pixels per cell, because the code has to reach the camera as pixels
 * rather than as a resampled approximation of pixels.
 */
function fitCellSize(grid, box) {
  const cells = grid + 2 * QUIET_ZONE_CELLS;
  const ratio = window.devicePixelRatio || 1;
  const shortest = Math.min(box.width, box.height) * ratio * FIT_MARGIN;
  return Math.max(3, Math.floor(shortest / cells));
}

/** Whether a profile can be drawn on this screen at a size a camera can read. */
function fitsOnScreen(profile, box) {
  return fitCellSize(profile.grid, box) >= profile.minCellPx;
}

/** The box the code will be drawn into, in CSS pixels. */
function stageBox() {
  const rect = ui.stage.getBoundingClientRect();
  if (rect.width > 1 && rect.height > 1) {
    return { width: rect.width, height: rect.height };
  }
  // The stage is still hidden, so fall back to the screen: the stage goes
  // fullscreen, and this is for the estimate shown before anything starts.
  return {
    width: Math.max(window.innerWidth, window.screen?.width ?? 0),
    height: Math.max(window.innerHeight, window.screen?.height ?? 0),
  };
}

/** Waits for layout to settle after a fullscreen change. */
function nextFrame() {
  return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
}

function describeProfile(profile, cellPx) {
  let text = t('profileFacts', {
    grid: profile.grid,
    bytes: profile.payloadCapacity,
    cellPx,
  });

  if (cellPx < profile.minCellPx) {
    return text + t('profileTooSmall', { cellPx, minimum: profile.minCellPx });
  }

  const perCell = cameraPixelsPerCell(profile);
  const needed = (profile.grid + 2 * QUIET_ZONE_CELLS) * perCell;
  const verdict = needed <= 640 ? 'verdictAny' : needed <= 1000 ? 'verdictFullHd' : 'verdict4k';
  text += t('profileNeeds', { perCell, needed, verdict: t(verdict) });
  return text;
}

/** Relabels the profiles this screen cannot draw legibly. */
function refreshProfileOptions() {
  const box = stageBox();
  PROFILES.forEach((profile, index) => {
    const option = ui.profile.options[index];
    if (!option) return;
    if (!fitsOnScreen(profile, box)) option.textContent = t('optionTooDense', { name: profile.name });
    else if (index === 0) option.textContent = t('optionRecommended', { name: profile.name });
    else option.textContent = profile.name;
  });
}

function refreshProfileNote() {
  const profile = PROFILES[ui.profile.selectedIndex];
  if (!profile) return;
  const cellPx = fitCellSize(profile.grid, stageBox());
  ui.profileNote.textContent = describeProfile(profile, cellPx);
  ui.profileNote.classList.toggle('bad', cellPx < profile.minCellPx);
  refreshEstimate();
}

function refreshRateNote() {
  const rate = Number(ui.rate.value);
  ui.rateValue.textContent = String(rate);
  ui.rateNote.textContent = t('rateNote', { ms: Math.round(1000 / rate) });
  refreshEstimate();
}

/** Says how long the chosen file will take, before anything is sent. */
function refreshEstimate() {
  const file = ui.file.files?.[0];
  const profile = PROFILES[ui.profile.selectedIndex];
  if (!file || !profile) {
    ui.estimate.textContent = '';
    return;
  }
  const rate = Number(ui.rate.value);
  const codes = Math.ceil(file.size / (profile.payloadCapacity * 0.95));
  ui.estimate.textContent = t('estimate', {
    size: bytes(file.size),
    rate,
    seconds: Math.max(1, Math.ceil(codes / rate)),
  });
}

/**
 * Measures how often this display refreshes.
 *
 * It cannot be assumed. Sixty hertz is common and so are 75, 120, 144 and 165,
 * and a code held for "four refreshes" is on screen for 67 ms on the first and
 * 24 ms on the last — which no phone camera can photograph whole. The interval
 * between animation frames is the refresh interval, so it is measured.
 */
async function measureRefresh() {
  const stamps = [];
  await new Promise((resolve) => {
    const tick = (now) => {
      stamps.push(now);
      if (stamps.length < 24) requestAnimationFrame(tick);
      else resolve();
    };
    requestAnimationFrame(tick);
  });

  const gaps = [];
  for (let index = 1; index < stamps.length; index += 1) {
    gaps.push(stamps[index] - stamps[index - 1]);
  }
  gaps.sort((a, b) => a - b);
  const median = gaps[Math.floor(gaps.length / 2)] || 16.7;
  // Between 24 and 360 Hz. Outside that the measurement is wrong, not the
  // display.
  return Math.min(360, Math.max(24, 1000 / median));
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
  if (cellPx < profile.minCellPx) {
    const alternative = PROFILES.find((other) => fitsOnScreen(other, box));
    ui.stage.classList.remove('showing');
    await exitFullscreen();
    say(
      alternative
        ? t('refuseProfile', {
            name: profile.name,
            cellPx,
            minimum: profile.minCellPx,
            alternative: alternative.name,
          })
        : t('refuseScreen'),
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
    await exitFullscreen();
    say(t('cannotSend', { error: String(error) }), 'bad');
    return;
  }

  const manifest = JSON.parse(emitter.manifest());
  const side = emitter.side();
  const refresh = await measureRefresh();

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
      t('clipped', {
        needed: Math.ceil(cssSide),
        available: Math.floor(Math.min(box.width, box.height)),
      }),
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
    profile,
    name: manifest.name,
    refresh,
    perPass: emitter.framesPerPass(),
    frames: 0,
    began: performance.now(),
    // When the code now showing went up, and when the next one is due.
    due: 0,
    paintMs: 0,
    lastLog: 0,
    raf: 0,
  };

  ui.facts.name.textContent = manifest.name;
  ui.facts.size.textContent = bytes(manifest.originalSize);
  ui.facts.compression.textContent = manifest.compression;
  ui.facts.frame.textContent = t('frameFact', { side, cellPx });
  ui.facts.pass.textContent = t('passFact', {
    frames: running.perPass,
    seconds: Math.ceil(running.perPass / Number(ui.rate.value)),
  });
  ui.summary.classList.remove('hidden');

  running.raf = requestAnimationFrame(paint);
}

/**
 * How long each code stays up, in milliseconds.
 *
 * A whole number of refreshes, never fewer than two, nearest to what was asked
 * for. Tying the change to the refresh is what keeps a code on screen for whole
 * refreshes; a code swapped between them would be shown for a fraction of one.
 */
function holdFor(state) {
  const interval = 1000 / state.refresh;
  const refreshes = Math.max(2, Math.round(1000 / Number(ui.rate.value) / interval));
  return refreshes * interval;
}

/** Paints a new code whenever the one showing has been up long enough. */
function paint(now) {
  if (!running) return;
  running.raf = requestAnimationFrame(paint);

  // Half a refresh of slack, so a callback that arrives a hair early does not
  // leave the code up for a whole extra refresh.
  const slack = 500 / running.refresh;
  if (running.frames > 0 && now < running.due - slack) return;

  const began = performance.now();
  let rgba;
  try {
    rgba = running.emitter.nextFrame();
  } catch (error) {
    stop();
    say(t('stopped', { error: String(error) }), 'bad');
    return;
  }

  const picture = new ImageData(
    new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength),
    running.side,
    running.side,
  );
  running.context.putImageData(picture, 0, 0);

  const spent = performance.now() - began;
  running.paintMs = running.paintMs === 0 ? spent : running.paintMs * 0.9 + spent * 0.1;

  if (CAPTURING && running.frames < CAPTURE_LIMIT) {
    sendPainted(running);
  }

  // From when this one was due rather than from now, so that being late once
  // does not make every later code late as well.
  const hold = holdFor(running);
  running.due = running.frames === 0 || now - running.due > hold ? now + hold : running.due + hold;

  running.frames += 1;
  const pass = Math.floor((running.frames - 1) / Math.max(1, running.perPass)) + 1;
  const elapsed = (now - running.began) / 1000;
  const rate = elapsed > 1 ? running.frames / elapsed : 1000 / hold;
  ui.stageStatus.textContent = t('stage', {
    name: running.name,
    frame: running.frames,
    pass,
    rate: rate.toFixed(1),
  });

  window.photonStats = {
    frames: running.frames,
    perSecond: Number(rate.toFixed(2)),
    refresh: Number(running.refresh.toFixed(1)),
    paintMs: Number(running.paintMs.toFixed(1)),
    cellPx: running.cellPx,
    side: running.side,
  };
  sendLog(now, rate);
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

/** Sends this page's measurements to the development server. */
function sendLog(now, rate) {
  if (!CAPTURING || now - running.lastLog < LOG_INTERVAL_MS) return;
  running.lastLog = now;

  fetch('/log', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      role: 'sender',
      seconds: Number(((now - running.began) / 1000).toFixed(1)),
      profile: running.profile.name,
      cellPx: running.cellPx,
      side: running.side,
      ratio: window.devicePixelRatio,
      refresh: Number(running.refresh.toFixed(1)),
      perSecond: Number(rate.toFixed(1)),
      paintMs: Number(running.paintMs.toFixed(1)),
      frames: running.frames,
      agent: navigator.userAgent,
    }),
  }).catch(() => {
    // As above.
  });
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

translatePage(t);

if (CAPTURING) {
  const banner = document.createElement('div');
  banner.className = 'status bad';
  banner.textContent = t('captureBanner', { limit: CAPTURE_LIMIT, origin: location.origin });
  document.body.prepend(banner);
}

ui.rate.addEventListener('input', refreshRateNote);
ui.profile.addEventListener('change', refreshProfileNote);
ui.file.addEventListener('change', () => {
  ui.start.disabled = !ui.file.files?.length;
  refreshEstimate();
});
ui.start.addEventListener('click', start);
ui.stop.addEventListener('click', stop);

document.addEventListener('fullscreenchange', () => {
  if (!document.fullscreenElement && running) stop();
});

// Which profiles fit is a property of the window, and the window changes.
window.addEventListener('resize', () => {
  if (running || !PROFILES.length) return;
  refreshProfileOptions();
  refreshProfileNote();
});

try {
  await init();
  // In order of what a code carries, which is not the order they were
  // numbered in.
  PROFILES = JSON.parse(profiles()).sort((a, b) => a.payloadCapacity - b.payloadCapacity);

  for (let index = 0; index < PROFILES.length; index += 1) {
    ui.profile.append(document.createElement('option'));
  }
  refreshProfileOptions();
  // The sparsest profile, whatever the screen could draw. What limits a
  // transfer is the camera at the other end, which this page cannot see, and
  // a code too dense for it looks exactly like one that is not. The denser
  // profiles are there to be chosen by someone who knows their camera.
  ui.profile.selectedIndex = 0;
  refreshProfileNote();
  refreshRateNote();
} catch (error) {
  say(t('moduleFailed', { error: String(error) }), 'bad');
  ui.start.disabled = true;
}
