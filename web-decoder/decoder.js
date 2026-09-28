// The receiving half. Takes frames from the camera or from a recording, hands
// them to a worker, and shows what the protocol makes of them.
//
// Frames come from a `<video>` element either way — a live stream or a file —
// so one pipeline serves both.
//
// Frames arrive in real time and are dropped whenever the decoder is busy. For
// most formats that would be a problem. Here it is the assumption the protocol
// was built on: any large enough subset of the stream rebuilds the file, so a
// skipped frame costs time and nothing else. What it does mean is that the
// throughput of the whole protocol is the number of pictures this page gets
// through in a second, and most of what follows is about that.

import init from '../photon/photon_wasm.js';
import { translator, translatePage } from '../shared/i18n.js';

const t = translator({
  title: { en: 'PhotonProtocol — receive', pt: 'PhotonProtocol — receber' },
  heading: { en: 'Receive a file', pt: 'Receber um arquivo' },
  tagline: {
    en: "Point this device's camera at the screen that is sending.",
    pt: 'Aponte a câmera deste aparelho para a tela que está enviando.',
  },
  start: { en: 'Start the camera', pt: 'Ligar a câmera' },
  startFile: { en: 'Read the recording', pt: 'Ler a gravação' },
  stop: { en: 'Stop', pt: 'Parar' },
  detailsSummary: { en: 'Details and settings', pt: 'Detalhes e ajustes' },
  factCamera: { en: 'Camera', pt: 'Câmera' },
  factBlocks: { en: 'Blocks', pt: 'Blocos' },
  factRate: { en: 'Rate', pt: 'Taxa' },
  factPerCell: { en: 'Pixels per cell', pt: 'Pixels por célula' },
  factSpeed: { en: 'Pictures read', pt: 'Imagens lidas' },
  factUsed: { en: 'New codes', pt: 'Códigos novos' },
  factRepeated: { en: 'Codes seen again', pt: 'Códigos repetidos' },
  factMissed: { en: 'Code not found', pt: 'Código não encontrado' },
  factStraddled: { en: 'Caught mid-change', pt: 'Pego no meio da troca' },
  factHeader: { en: 'Header unreadable', pt: 'Cabeçalho ilegível' },
  factCells: { en: 'Cells unreadable', pt: 'Células ilegíveis' },
  factTimes: { en: 'Time per picture', pt: 'Tempo por imagem' },
  factFile: { en: 'File', pt: 'Arquivo' },
  qualityLabel: { en: 'Camera resolution', pt: 'Resolução da câmera' },
  quality1080: { en: '1080p — recommended', pt: '1080p — recomendado' },
  quality2160: { en: '4K — sharper, slower', pt: '4K — mais nítido, mais lento' },
  quality720: { en: '720p — for a slow phone', pt: '720p — para celular lento' },
  useRecording: { en: 'Read a recording instead', pt: 'Ler uma gravação em vez da câmera' },
  useCamera: { en: 'Use the camera instead', pt: 'Usar a câmera' },
  recordingLabel: { en: 'A video of the sending screen', pt: 'Um vídeo da tela que envia' },
  privacy: {
    en: 'Nothing is uploaded. Everything is read on this device.',
    pt: 'Nada é enviado para a internet. Tudo é lido neste aparelho.',
  },
  tipsHeading: { en: 'For a quick transfer', pt: 'Para transferir rápido' },
  tipFill: {
    en: 'Get close enough for the code to fill the width of the picture.',
    pt: 'Chegue perto até o código ocupar a largura da imagem.',
  },
  tipSteady: {
    en: 'Hold still, square on to the screen.',
    pt: 'Segure firme, de frente para a tela.',
  },
  tipGlare: {
    en: 'Keep lamps and windows from reflecting on the screen.',
    pt: 'Evite reflexo de lâmpada ou janela na tela.',
  },
  tipBright: {
    en: "Turn the sending screen's brightness up.",
    pt: 'Aumente o brilho da tela que envia.',
  },
  back: { en: 'Back', pt: 'Voltar' },
  sendInstead: { en: 'Send a file instead', pt: 'Enviar um arquivo' },

  cameraOpen: {
    en: 'Camera open at {width}×{height}. Point it at the other screen.',
    pt: 'Câmera aberta em {width}×{height}. Aponte para a outra tela.',
  },
  cameraFailed: {
    en: 'The camera could not be opened: {error}',
    pt: 'Não foi possível abrir a câmera: {error}',
  },
  cameraInsecure: {
    en: 'This browser only offers the camera to pages loaded over https. Open this page by its https address.',
    pt: 'Este navegador só libera a câmera para páginas abertas por https. Abra esta página pelo endereço https.',
  },
  noRecording: { en: 'No recording was chosen.', pt: 'Nenhuma gravação foi escolhida.' },
  readingFile: {
    en: 'Reading {width}×{height}. This takes a while.',
    pt: 'Lendo {width}×{height}. Isso demora um pouco.',
  },
  fileEnded: {
    en: 'The recording ended. Working out what was recovered…',
    pt: 'A gravação terminou. Verificando o que foi recuperado…',
  },
  fileUnreadable: {
    en: 'The browser could not read this video file.',
    pt: 'O navegador não conseguiu ler este vídeo.',
  },
  stopping: {
    en: 'Stopping. Working out what was recovered…',
    pt: 'Parando. Verificando o que foi recuperado…',
  },
  recovered: {
    en: 'Received {name} — {size}, in {seconds} s ({rate}/s). The digest matches: it is exactly what was sent.',
    pt: 'Recebido {name} — {size}, em {seconds} s ({rate}/s). O hash confere: é exatamente o que foi enviado.',
  },
  save: { en: 'Save {name}', pt: 'Salvar {name}' },
  moduleFailed: {
    en: 'The protocol module failed to load: {error}',
    pt: 'O módulo do protocolo não carregou: {error}',
  },
  failure: { en: 'It did not finish: {message}', pt: 'Não terminou: {message}' },

  headlineSearching: { en: 'Looking for a code…', pt: 'Procurando um código…' },
  headlineWaiting: {
    en: 'Code found. Reading its first block…',
    pt: 'Código encontrado. Lendo o primeiro bloco…',
  },
  headlineProgress: {
    en: '{percent}% — {name}, {rate}/s, about {remaining} s to go',
    pt: '{percent}% — {name}, {rate}/s, faltam uns {remaining} s',
  },
  headlineFinishing: { en: 'Putting the file together…', pt: 'Montando o arquivo…' },

  blocks: { en: '{accepted} of about {needed}', pt: '{accepted} de cerca de {needed}' },
  times: {
    en: '{grab} ms to grab, {decode} ms to read, {fps} a second',
    pt: '{grab} ms para capturar, {decode} ms para ler, {fps} por segundo',
  },

  aimNothing: {
    en: 'No code in sight. Point the camera at the sending screen and keep the whole code in the picture.',
    pt: 'Nenhum código à vista. Aponte a câmera para a tela que envia e mantenha o código inteiro na imagem.',
  },
  aimPartial: {
    en: 'Part of the code is cut off. Get all four corners into the picture.',
    pt: 'Parte do código está cortada. Coloque os quatro cantos dentro da imagem.',
  },
  aimCorners: {
    en: 'The corners are in sight but the code will not read. Hold steadier and move any reflection off the screen.',
    pt: 'Os cantos aparecem, mas o código não lê. Segure mais firme e tire o reflexo da tela.',
  },
  aimCloser: {
    en: 'Too far: only {perCell} camera pixels per cell. Move closer, until the code fills the width of the picture.',
    pt: 'Longe demais: só {perCell} pixels da câmera por célula. Chegue mais perto, até o código ocupar a largura da imagem.',
  },
  aimTooFast: {
    en: 'The code is large enough in the picture, yet most pictures will not read. That is usually the screen changing codes faster than this camera can photograph one: on the sending screen, open Settings and lower the codes per second (try 6). If that does not help, move any reflection off the screen and hold steadier.',
    pt: 'O código está grande o bastante na imagem, mas a maioria das imagens não lê. Em geral é a tela trocando de código mais rápido do que esta câmera consegue fotografar: na tela que envia, abra Ajustes e diminua os códigos por segundo (tente 6). Se não resolver, tire qualquer reflexo da tela e segure mais firme.',
  },
  aimUnreadable: {
    en: 'The code is found but will not read. Get closer, hold steadier, and move any reflection off the screen.',
    pt: 'O código é encontrado, mas não lê. Chegue mais perto, segure firme e tire qualquer reflexo da tela.',
  },
  aimReading: {
    en: 'Reading. Keep the code in the picture; the file is offered by itself when it is complete.',
    pt: 'Lendo. Mantenha o código na imagem; o arquivo aparece sozinho quando estiver completo.',
  },
  hintCloser: { en: 'Move closer', pt: 'Chegue mais perto' },
  hintGood: { en: 'Reading', pt: 'Lendo' },
  hintSearching: { en: 'Point at the code', pt: 'Aponte para o código' },

  captureBanner: {
    en: 'Diagnostic capture is ON. Pictures this page cannot read, and its measurements, are sent to {origin}, the server this page came from. Remove "?capture" from the address to turn it off.',
    pt: 'Captura de diagnóstico LIGADA. As imagens que esta página não consegue ler, e as medições dela, são enviadas para {origin}, o servidor de onde esta página veio. Tire "?capture" do endereço para desligar.',
  },
  sampleLink: {
    en: 'Save a picture this could not read (for diagnosis)',
    pt: 'Salvar uma imagem que não foi lida (para diagnóstico)',
  },
});

const ui = {
  modeFile: document.getElementById('mode-file'),
  filePane: document.getElementById('file-pane'),
  viewer: document.getElementById('viewer'),
  preview: document.getElementById('preview'),
  overlay: document.getElementById('overlay'),
  hint: document.getElementById('hint'),
  video: document.getElementById('video'),
  quality: document.getElementById('quality'),
  start: document.getElementById('start'),
  stop: document.getElementById('stop'),
  live: document.getElementById('live'),
  bar: document.getElementById('bar'),
  headline: document.getElementById('headline'),
  aim: document.getElementById('aim'),
  sample: document.getElementById('sample'),
  sampleRow: document.getElementById('sample-row'),
  status: document.getElementById('status'),
  result: document.getElementById('result'),
  picture: document.getElementById('picture'),
  download: document.getElementById('download'),
  player: document.getElementById('player'),
  grabber: document.getElementById('grabber'),
  facts: {
    camera: document.getElementById('fact-camera'),
    blocks: document.getElementById('fact-blocks'),
    rate: document.getElementById('fact-rate'),
    perCell: document.getElementById('fact-percell'),
    frames: document.getElementById('fact-frames'),
    used: document.getElementById('fact-used'),
    repeated: document.getElementById('fact-repeated'),
    missed: document.getElementById('fact-missed'),
    straddled: document.getElementById('fact-straddled'),
    header: document.getElementById('fact-header'),
    cells: document.getElementById('fact-cells'),
    times: document.getElementById('fact-times'),
    name: document.getElementById('fact-name'),
  },
};

/** How much faster than real time to play a recorded file. */
const PLAYBACK_RATE = 1;

/**
 * Pixels per cell below which a capture is not going to work.
 *
 * The finest thing in a cell is half of it, and a camera needs a little over
 * two pixels to resolve anything. Measured with `photon film`, the
 * conservative profile still reads at five and a half and does not at four.
 */
const MINIMUM_PIXELS_PER_CELL = 5.5;

/**
 * Pixels per cell above which distance is not what is wrong.
 *
 * At this size the conservative profile reads without error through every
 * camera `photon film` models, so pictures that still will not read are being
 * spoilt by something else.
 */
const COMFORTABLE_PIXELS_PER_CELL = 7;

/**
 * How much of the picture round the code to read, as a fraction of the code's
 * size.
 *
 * The corners reported are the middles of the corner patterns, and the code
 * and its white margin reach a twelfth of its width beyond them. The rest is
 * room for the hand to move before the next picture.
 */
const REGION_MARGIN = 0.2;

/** Pictures without a code before the whole picture is searched again. */
const LOST_AFTER = 3;

/**
 * Whether to send unreadable pictures back to the server that served this page.
 *
 * Off unless the address says `?capture`, and deliberately so. This page tells
 * people nothing is uploaded, and that has to stay true by default — a
 * diagnostic that turns itself on quietly would make the promise a lie. The
 * development server is the only thing that accepts these; the deployed pages
 * have nowhere to send them.
 */
const CAPTURING = new URLSearchParams(location.search).has('capture');

/**
 * How many times slower than it is to pretend this device is, from `?slow=`.
 *
 * For testing on a desk what a phone will do. A desktop reads a picture several
 * times faster than a phone does, and how many pictures a second get read is
 * what decides the throughput, so a test at desktop speed flatters every
 * number. The worker spends the extra time doing nothing.
 */
const SLOWDOWN = Math.max(1, Number(new URLSearchParams(location.search).get('slow')) || 1);

/** Longest run of pictures to send, and the gap between them. */
const CAPTURE_LIMIT = 40;
const CAPTURE_INTERVAL_MS = 700;

/** How often measurements are sent while capturing. */
const LOG_INTERVAL_MS = 2000;

/**
 * How many pictures to read at once.
 *
 * One fewer than the cores there are, less another for this page itself, and
 * no more than four: past that the camera is the limit, not the reading.
 * `?readers=` overrides it, for measuring what each one is worth.
 */
const READERS = (() => {
  const asked = Number(new URLSearchParams(location.search).get('readers'));
  if (asked >= 1) return Math.min(8, Math.floor(asked));
  const cores = navigator.hardwareConcurrency || 4;
  return Math.min(4, Math.max(1, cores - 2));
})();

/** Codes recently read that a reader is told not to read again. */
const REMEMBERED = 96;

let readers = [];
let collector = null;
let session = null;
let mode = 'camera';

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

function setMode(next) {
  mode = next;
  ui.filePane.classList.toggle('hidden', next !== 'file');
  ui.modeFile.textContent = next === 'file' ? t('useCamera') : t('useRecording');
  ui.start.textContent = next === 'camera' ? t('start') : t('startFile');
  ui.start.disabled = next === 'file' && !ui.video.files?.length;
}

function reset() {
  ui.sampleRow.style.display = 'none';
  ui.result.classList.add('hidden');
  ui.picture.classList.add('hidden');
  ui.status.classList.add('hidden');
  ui.aim.textContent = '';
  ui.hint.textContent = '';
  ui.headline.textContent = t('headlineSearching');
  ui.bar.value = 0;
  for (const node of Object.values(ui.facts)) node.textContent = '—';
}

/** Starts reading, from whichever source is selected. */
async function start() {
  reset();
  ui.live.classList.remove('hidden');
  ui.start.disabled = true;
  ui.start.classList.add('hidden');
  ui.stop.classList.remove('hidden');

  session = {
    began: performance.now(),
    firstSymbol: null,
    frames: 0,
    used: 0,
    repeated: 0,
    missed: 0,
    straddled: 0,
    headerLost: 0,
    cellsLost: 0,
    sampled: false,
    perCell: null,
    diagnosis: null,
    accepted: 0,
    needed: 0,
    name: '',
    // The transfer this receiver has locked onto, once it has one.
    locked: null,
    // Codes already read, newest last, and codes being read this moment.
    decoded: [],
    reading: new Map(),
    nextId: 0,
    // Pictures handed to the collector and not yet answered for.
    awaiting: new Map(),
    // Where the code was last seen, in the video's own pixels.
    corners: null,
    lost: 0,
    region: null,
    // Smoothed timings, for the display and for the log.
    grabMs: 0,
    decodeMs: 0,
    perSecond: 0,
    lastTick: performance.now(),
    ticks: 0,
    pending: null,
    captured: 0,
    lastCapture: 0,
    lastLog: 0,
    finished: false,
    url: null,
    stream: null,
    source: null,
    size: { width: 0, height: 0 },
  };

  const spawn = (role, onMessage) => {
    const worker = new Worker(new URL('./decoder-worker.js', import.meta.url), { type: 'module' });
    const entry = { worker, busy: false, ready: false };
    worker.onmessage = (event) => onMessage(entry, event.data);
    worker.onerror = (event) => concludeFailure(event.message ?? 'the decoder stopped');
    worker.postMessage({ type: 'start', role, slowdown: SLOWDOWN });
    return entry;
  };

  collector = spawn('collector', onCollectorMessage);
  readers = Array.from({ length: READERS }, () => spawn('reader', onReaderMessage));
}

/** Opens the source once every worker has loaded the protocol. */
function beginWhenReady() {
  if (!session || session.source) return;
  if (!collector?.ready || !readers.every((reader) => reader.ready)) return;
  session.source = 'opening';
  if (mode === 'camera') openCamera();
  else playFile();
}

/** Codes a reader should not spend time on: read already, or being read. */
function taken() {
  return [...session.decoded, ...session.reading.values()];
}

function onReaderMessage(reader, message) {
  if (!session) return;

  switch (message.type) {
    case 'ready':
      reader.ready = true;
      beginWhenReady();
      break;

    case 'opened':
      session.reading.set(message.id, message.sequence);
      break;

    case 'read': {
      reader.busy = false;
      const { summary, region } = message;

      if (message.reading) {
        // Read as far as it goes here. Whether it decoded, and what it carried,
        // is for the collector to say.
        session.awaiting.set(message.id, { summary, region });
        collector.worker.postMessage(
          { type: 'absorb', id: message.id, reading: message.reading },
          [message.reading],
        );
      } else {
        session.reading.delete(message.id);
        applyReport(summary, region);
        sendCapture(summary);
      }
      break;
    }

    case 'failed':
      reader.busy = false;
      concludeFailure(message.message);
      break;

    default:
      break;
  }
}

function onCollectorMessage(entry, message) {
  if (!session) return;

  switch (message.type) {
    case 'ready':
      entry.ready = true;
      beginWhenReady();
      break;

    case 'report': {
      const waiting = session.awaiting.get(message.id);
      session.awaiting.delete(message.id);
      session.reading.delete(message.id);
      if (!waiting) return;

      // What the reader measured, with what the collector made of it.
      const report = { ...waiting.summary, ...message.report };
      if (report.outcome === 'decoded') {
        session.locked ??= waiting.summary.session;
        session.decoded.push(report.sequence);
        if (session.decoded.length > REMEMBERED) session.decoded.shift();
      }
      applyReport(report, waiting.region);
      sendCapture(report);
      break;
    }

    case 'done':
      finishWith(message.name, message.bytes);
      break;

    case 'failed':
      concludeFailure(message.message);
      break;

    default:
      break;
  }
}

function applyReport(report, region) {
  // Every outcome is counted. Showing only "found" and "not found" left the
  // most common real failure invisible: hundreds of frames located, none used,
  // and nothing on screen to say what had happened to them.
  switch (report.outcome) {
    case 'notLocated':
      session.missed += 1;
      break;
    case 'straddled':
      session.straddled += 1;
      break;
    case 'headerUnreadable':
      session.headerLost += 1;
      break;
    case 'payloadUnrecoverable':
      session.cellsLost += 1;
      break;
    case 'duplicate':
      session.repeated += 1;
      break;
    case 'decoded':
      session.used += 1;
      break;
    default:
      break;
  }

  // Where the code is, in the video's own pixels: what the decoder reported,
  // moved by where the region it was given came from.
  if (report.corners) {
    session.corners = report.corners.map(([x, y]) => [x + region.x, y + region.y]);
    session.lost = 0;
  } else {
    session.lost += 1;
    if (session.lost >= LOST_AFTER) session.corners = null;
  }

  if (typeof report.pixelsPerCell === 'number') session.perCell = report.pixelsPerCell;
  if (report.diagnosis) session.diagnosis = report.diagnosis;

  if (typeof report.decodeMs === 'number') {
    session.decodeMs = smooth(session.decodeMs, report.decodeMs);
  }

  if (report.needed > 0) {
    session.accepted = report.accepted;
    session.needed = report.needed;
    if (session.firstSymbol === null) session.firstSymbol = performance.now();
  }
  if (report.name) session.name = report.name;
  if (report.size) session.fileSize = report.size;

  // How many pictures a second are getting through, which is what bounds the
  // throughput of the whole transfer.
  session.ticks += 1;
  const now = performance.now();
  if (now - session.lastTick >= 1000) {
    session.perSecond = (session.ticks * 1000) / (now - session.lastTick);
    session.ticks = 0;
    session.lastTick = now;
  }

  render();
  drawOverlay(report);
  updateAim();
  sendLog();

  window.photonStats = {
    grabMs: Math.round(session.grabMs),
    decodeMs: Math.round(session.decodeMs),
    perSecond: Number(session.perSecond.toFixed(1)),
    region: session.region ? `${session.region.w}x${session.region.h}` : '',
    readers: READERS,
  };
}

function smooth(previous, value) {
  return previous === 0 ? value : previous * 0.85 + value * 0.15;
}

/** Bytes a second since the first block arrived, and the seconds left at that rate. */
function progress() {
  if (!session.needed || session.firstSymbol === null) return null;
  const fraction = Math.min(1, session.accepted / session.needed);
  const elapsed = (performance.now() - session.firstSymbol) / 1000;
  const size = session.fileSize ?? 0;
  const rate = elapsed > 0.5 ? (fraction * size) / elapsed : 0;
  const remaining = rate > 0 ? ((1 - fraction) * size) / rate : 0;
  return { fraction, rate, remaining };
}

function render() {
  ui.facts.frames.textContent = String(session.frames);
  ui.facts.used.textContent = String(session.used);
  ui.facts.repeated.textContent = String(session.repeated);
  ui.facts.missed.textContent = String(session.missed);
  ui.facts.straddled.textContent = String(session.straddled);
  ui.facts.header.textContent = String(session.headerLost);
  ui.facts.cells.textContent = String(session.cellsLost);
  ui.facts.perCell.textContent = session.perCell === null ? '—' : session.perCell.toFixed(1);
  ui.facts.times.textContent = t('times', {
    grab: session.grabMs.toFixed(0),
    decode: session.decodeMs.toFixed(0),
    fps: session.perSecond.toFixed(1),
  });
  if (session.name) ui.facts.name.textContent = session.name;

  const state = progress();
  if (!state) {
    ui.headline.textContent = session.corners ? t('headlineWaiting') : t('headlineSearching');
    return;
  }

  ui.bar.max = session.needed;
  ui.bar.value = Math.min(session.accepted, session.needed);
  ui.facts.blocks.textContent = t('blocks', { accepted: session.accepted, needed: session.needed });
  ui.facts.rate.textContent = `${bytes(Math.round(state.rate))}/s`;

  ui.headline.textContent =
    state.fraction >= 1
      ? t('headlineFinishing')
      : t('headlineProgress', {
          percent: Math.floor(state.fraction * 100),
          name: session.name || '…',
          rate: bytes(Math.round(state.rate)),
          remaining: Math.max(1, Math.round(state.remaining)),
        });
}

/** Outlines the code on the preview, so the person aiming can see it was found. */
function drawOverlay(report) {
  const canvas = ui.overlay;
  const { width, height } = session.size;
  if (!width || !height) return;
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }

  const context = canvas.getContext('2d');
  context.clearRect(0, 0, width, height);

  const close = session.perCell !== null && session.perCell >= MINIMUM_PIXELS_PER_CELL;
  if (!session.corners) {
    ui.hint.textContent = t('hintSearching');
    ui.hint.className = 'viewer-hint';
    return;
  }

  const good = report.outcome === 'decoded' || report.outcome === 'duplicate';
  context.lineWidth = Math.max(4, width / 160);
  context.strokeStyle = !close ? '#ffb020' : good ? '#35d07f' : '#ffffff';
  context.beginPath();
  session.corners.forEach(([x, y], index) => {
    if (index === 0) context.moveTo(x, y);
    else context.lineTo(x, y);
  });
  context.closePath();
  context.stroke();

  ui.hint.textContent = close ? t('hintGood') : t('hintCloser');
  ui.hint.className = `viewer-hint ${close ? 'good' : 'warn'}`;
}

/**
 * Says the one thing the person holding the camera can act on.
 *
 * Pixels per cell first, because it is the only failure extra filming does not
 * fix, and because it is the only one they can change by moving.
 */
function updateAim() {
  const read = session.used + session.repeated;
  // A picture taken while the screen was changing fails wherever the change
  // happened to fall — at the header, in the cells, or with two headers that
  // disagree — so the three are counted together.
  const unread = session.straddled + session.headerLost + session.cellsLost;

  if (session.perCell !== null && session.perCell < MINIMUM_PIXELS_PER_CELL) {
    ui.aim.textContent = t('aimCloser', { perCell: session.perCell.toFixed(1) });
    return;
  }
  if (unread > 12 && unread > 3 * read) {
    const large = session.perCell !== null && session.perCell >= COMFORTABLE_PIXELS_PER_CELL;
    ui.aim.textContent = t(large ? 'aimTooFast' : 'aimUnreadable');
    return;
  }
  if (session.frames > 12 && session.used === 0 && !session.corners) {
    const found = session.diagnosis?.finderCandidates ?? 0;
    if (found === 0) ui.aim.textContent = t('aimNothing');
    else if (!session.diagnosis?.quadFound) ui.aim.textContent = t('aimPartial');
    else ui.aim.textContent = t('aimCorners');
    return;
  }
  ui.aim.textContent = session.used > 0 ? t('aimReading') : '';
}

/**
 * Sends one picture, and what the decoder made of it, to the development
 * server.
 *
 * Only pictures that failed. A picture that decoded needs no explaining, and
 * the point of this is to put the actual frames in front of whoever is trying
 * to work out why the others did not.
 */
async function sendCapture(report) {
  if (!CAPTURING || !session.pending) return;

  const blob = await session.pending;
  session.pending = null;
  const fine = report.outcome === 'decoded' || report.outcome === 'duplicate';
  if (!blob || fine || session.captured >= CAPTURE_LIMIT) return;

  session.captured += 1;
  const query = new URLSearchParams({
    outcome: report.outcome,
    report: JSON.stringify(report),
  });

  try {
    await fetch(`/capture?${query}`, { method: 'POST', body: blob });
  } catch {
    // The server is not listening for these, which is the normal case. Nothing
    // about the transfer depends on it.
  }
}

/**
 * Sends this page's measurements to the development server.
 *
 * What a phone actually did is the one thing that cannot be found out from a
 * desk: what resolution the camera really gave, how long a picture really took
 * to read, which stage the pictures really stopped at. A line of numbers every
 * couple of seconds answers all three.
 */
function sendLog(final = null) {
  if (!CAPTURING || !session) return;
  const now = performance.now();
  if (!final && now - session.lastLog < LOG_INTERVAL_MS) return;
  session.lastLog = now;

  const state = progress();
  const entry = {
    seconds: Number(((now - session.began) / 1000).toFixed(1)),
    camera: `${session.size.width}x${session.size.height}`,
    region: session.region ? `${session.region.w}x${session.region.h}` : null,
    perCell: session.perCell,
    perSecond: Number(session.perSecond.toFixed(1)),
    grabMs: Math.round(session.grabMs),
    decodeMs: Math.round(session.decodeMs),
    readers: READERS,
    frames: session.frames,
    used: session.used,
    repeated: session.repeated,
    missed: session.missed,
    straddled: session.straddled,
    headerLost: session.headerLost,
    cellsLost: session.cellsLost,
    accepted: session.accepted,
    needed: session.needed,
    rate: state ? Math.round(state.rate) : 0,
    final,
    agent: navigator.userAgent,
  };

  fetch('/log', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(entry),
  }).catch(() => {
    // As above: nothing depends on this landing.
  });
}

/** Live capture. */
async function openCamera() {
  if (!navigator.mediaDevices?.getUserMedia) {
    concludeFailure(t('cameraInsecure'), true);
    return;
  }

  // Full HD unless asked otherwise. Asking for the most the camera has, as
  // this page used to, gets four times the pixels on a recent phone — four
  // times the work for every picture, for cells that were already large enough
  // to read — and what decides the throughput is pictures a second.
  const long = { 720: 1280, 1080: 1920, 2160: 3840 }[ui.quality.value] ?? 1920;
  const short = Number(ui.quality.value) || 1080;

  try {
    session.stream = await navigator.mediaDevices.getUserMedia({
      video: {
        facingMode: { ideal: 'environment' },
        width: { ideal: long },
        height: { ideal: short },
        frameRate: { ideal: 30 },
      },
      audio: false,
    });
  } catch (error) {
    concludeFailure(t('cameraFailed', { error: error?.message ?? String(error) }), true);
    return;
  }

  const track = session.stream.getVideoTracks()[0];
  await sharpen(track);

  ui.viewer.classList.remove('hidden');
  ui.preview.srcObject = session.stream;
  await ui.preview.play().catch(() => {});

  const width = ui.preview.videoWidth;
  const height = ui.preview.videoHeight;
  session.size = { width, height };
  ui.facts.camera.textContent = `${width}×${height}`;
  say(t('cameraOpen', { width, height }));

  pump(ui.preview);
}

/**
 * Asks the camera to keep focusing, where it lets a page ask.
 *
 * A screen at arm's length is nearer than most cameras rest at, and a camera
 * left in single-shot focus settles once on whatever it saw first.
 */
async function sharpen(track) {
  try {
    const capabilities = track.getCapabilities?.() ?? {};
    if (capabilities.focusMode?.includes?.('continuous')) {
      await track.applyConstraints({ advanced: [{ focusMode: 'continuous' }] });
    }
  } catch {
    // Most browsers do not offer this. The camera's own default is what is
    // left, and it is usually continuous already.
  }
}

/** A recording already on the device. */
function playFile() {
  const file = ui.video.files?.[0];
  if (!file) {
    concludeFailure(t('noRecording'), true);
    return;
  }

  session.url = URL.createObjectURL(file);
  const player = ui.player;
  player.src = session.url;
  player.playbackRate = PLAYBACK_RATE;

  player.onloadedmetadata = () => {
    session.size = { width: player.videoWidth, height: player.videoHeight };
    ui.facts.camera.textContent = `${player.videoWidth}×${player.videoHeight}`;
    say(t('readingFile', { width: player.videoWidth, height: player.videoHeight }));
  };

  player.onended = () => {
    if (!session.finished) {
      say(t('fileEnded'));
      collector?.worker.postMessage({ type: 'finish' });
    }
  };

  player.onerror = () => concludeFailure(t('fileUnreadable'), true);

  pump(player);
  player.play().catch((error) => concludeFailure(String(error)));
}

/** Feeds presented frames to the worker, from either source. */
function pump(element) {
  session.source = element;

  if ('requestVideoFrameCallback' in element) {
    const step = () => {
      if (!session || session.finished) return;
      grab(element);
      element.requestVideoFrameCallback(step);
    };
    element.requestVideoFrameCallback(step);
    return;
  }

  // Older browsers present no per-frame callback. Sampling on a timer sees
  // fewer frames, which costs time rather than correctness.
  const timer = setInterval(() => {
    if (!session || session.finished) {
      clearInterval(timer);
      return;
    }
    grab(element);
  }, 33);
}

/**
 * The part of the picture worth reading: round where the code last was, or all
 * of it when the code has not been seen lately.
 *
 * Most of a picture of a screen is not the code — it is the wall, the desk and
 * the rest of the monitor — and reading a picture costs in proportion to its
 * pixels, first to copy them out of the video and then to search them.
 */
function regionFor(width, height) {
  if (!session.corners) return { x: 0, y: 0, w: width, h: height };

  const xs = session.corners.map(([x]) => x);
  const ys = session.corners.map(([, y]) => y);
  const [left, right] = [Math.min(...xs), Math.max(...xs)];
  const [top, bottom] = [Math.min(...ys), Math.max(...ys)];
  const margin = Math.max(right - left, bottom - top) * REGION_MARGIN;

  const x = Math.max(0, Math.floor(left - margin));
  const y = Math.max(0, Math.floor(top - margin));
  const w = Math.min(width, Math.ceil(right + margin)) - x;
  const h = Math.min(height, Math.ceil(bottom + margin)) - y;
  if (w < 64 || h < 64) return { x: 0, y: 0, w: width, h: height };
  return { x, y, w, h };
}

/**
 * Sends the frame now on screen to the worker, unless it is still busy.
 *
 * Skipping rather than queueing is deliberate. A queue would grow without
 * bound, hold every frame's pixels in memory, and go on delivering stale frames
 * long after the file was already recoverable.
 */
function grab(element) {
  if (session.finished) return;
  const reader = readers.find((candidate) => candidate.ready && !candidate.busy);
  if (!reader) return;

  const width = element.videoWidth;
  const height = element.videoHeight;
  if (!width || !height) return;
  session.size = { width, height };

  const began = performance.now();
  const region = regionFor(width, height);
  session.region = region;

  if (ui.grabber.width !== region.w || ui.grabber.height !== region.h) {
    ui.grabber.width = region.w;
    ui.grabber.height = region.h;
  }
  const context = ui.grabber.getContext('2d', { willReadFrequently: true });
  context.drawImage(element, region.x, region.y, region.w, region.h, 0, 0, region.w, region.h);
  const picture = context.getImageData(0, 0, region.w, region.h);
  const buffer = picture.data.buffer;
  session.grabMs = smooth(session.grabMs, performance.now() - began);

  // Keep this exact picture until its report comes back. Grabbing one after the
  // fact would upload a later frame than the one being explained.
  if (CAPTURING && session.captured < CAPTURE_LIMIT) {
    const now = performance.now();
    if (now - session.lastCapture > CAPTURE_INTERVAL_MS) {
      session.lastCapture = now;
      session.pending = new Promise((resolve) => ui.grabber.toBlob(resolve, 'image/png'));
    }
  }

  reader.busy = true;
  session.frames += 1;
  const id = session.nextId;
  session.nextId += 1;

  // Ask for a diagnosis every so often, but only while nothing has worked yet.
  const diagnose = session.used === 0 && session.frames % 15 === 0;

  // Keep one picture that failed, exactly as the decoder saw it.
  if (!session.sampled && session.used === 0 && session.frames === 45) {
    session.sampled = true;
    ui.grabber.toBlob((blob) => {
      if (!blob) return;
      ui.sample.href = URL.createObjectURL(blob);
      ui.sample.download = 'photon-unread-frame.png';
      ui.sample.textContent = t('sampleLink');
      ui.sampleRow.style.display = '';
    }, 'image/png');
  }

  reader.worker.postMessage(
    {
      type: 'frame',
      id,
      buffer,
      width: region.w,
      height: region.h,
      region,
      diagnose,
      taken: taken(),
      session: session.locked,
    },
    [buffer],
  );
}

/** What kind of file a name says it is, for the ones a browser can show. */
function mediaType(name) {
  const extension = name.split('.').pop()?.toLowerCase() ?? '';
  return (
    {
      jpg: 'image/jpeg',
      jpeg: 'image/jpeg',
      png: 'image/png',
      gif: 'image/gif',
      webp: 'image/webp',
      svg: 'image/svg+xml',
      pdf: 'application/pdf',
      txt: 'text/plain',
      mp4: 'video/mp4',
      mp3: 'audio/mpeg',
    }[extension] ?? 'application/octet-stream'
  );
}

function finishWith(name, buffer) {
  session.finished = true;
  const seconds = (performance.now() - (session.firstSymbol ?? session.began)) / 1000;
  sendLog('received');
  stopSources();

  const content = new Uint8Array(buffer);
  const type = mediaType(name);
  const blob = new Blob([content], { type });
  const url = URL.createObjectURL(blob);

  ui.facts.name.textContent = `${name} (${bytes(content.length)})`;
  ui.download.href = url;
  ui.download.download = name || 'photon-received';
  ui.download.textContent = t('save', { name });
  ui.result.classList.remove('hidden');

  if (type.startsWith('image/')) {
    ui.picture.src = url;
    ui.picture.classList.remove('hidden');
  }

  ui.bar.max = 1;
  ui.bar.value = 1;
  ui.headline.textContent = '';
  ui.aim.textContent = '';
  ui.viewer.classList.add('hidden');

  say(
    t('recovered', {
      name,
      size: bytes(content.length),
      seconds: seconds.toFixed(1),
      rate: bytes(Math.round(content.length / Math.max(seconds, 0.1))),
    }),
    'good',
  );
  ui.start.disabled = false;
  ui.start.classList.remove('hidden');
  ui.stop.classList.add('hidden');

  // Offer it without waiting to be asked. Some browsers refuse a download
  // nobody tapped for; the link above is still there for them.
  try {
    ui.download.click();
  } catch {
    // The link is on the page either way.
  }
}

function concludeFailure(message, verbatim = false) {
  if (session) session.finished = true;
  sendLog(`failed: ${message}`);
  stopSources();

  say(verbatim ? message : t('failure', { message }), 'bad');
  ui.viewer.classList.add('hidden');
  ui.start.disabled = mode === 'file' && !ui.video.files?.length;
  ui.start.classList.remove('hidden');
  ui.stop.classList.add('hidden');
}

function stopSources() {
  ui.player.pause();
  ui.player.onended = null;
  session?.stream?.getTracks?.().forEach((track) => track.stop());
  ui.preview.srcObject = null;
  if (session?.url) URL.revokeObjectURL(session.url);
  for (const entry of [collector, ...readers]) entry?.worker.terminate();
  collector = null;
  readers = [];
}

function stop() {
  if (!session || session.finished) return;
  say(t('stopping'));
  ui.player.pause();
  session.stream?.getTracks?.().forEach((track) => track.stop());
  collector?.worker.postMessage({ type: 'finish' });
}

translatePage(t);

ui.modeFile.addEventListener('click', () => setMode(mode === 'file' ? 'camera' : 'file'));
ui.video.addEventListener('change', () => {
  ui.start.disabled = !ui.video.files?.length;
});
ui.start.addEventListener('click', start);
ui.stop.addEventListener('click', stop);

if (CAPTURING) {
  const banner = document.createElement('div');
  banner.className = 'status bad';
  banner.textContent = t('captureBanner', { origin: location.origin });
  document.body.prepend(banner);
}

try {
  await init();
  setMode('camera');
  ui.start.disabled = false;
} catch (error) {
  say(t('moduleFailed', { error: String(error) }), 'bad');
  ui.start.disabled = true;
}
