// The receiving half. Takes frames from the camera or from a recording, hands
// them to a worker, and shows what the protocol makes of them.
//
// Frames come from a `<video>` element either way — a live stream or a file —
// so one pipeline serves both. For files that means the browser's own hardware
// decoder does the container and codec work, which no JavaScript would do
// better, and it keeps a megabyte of demuxer out of the page.
//
// The consequence is that frames arrive in real time and are dropped whenever
// the decoder is busy. For most formats that would be a problem. Here it is the
// assumption the protocol was built on: any large enough subset of the stream
// rebuilds the file, so a skipped frame costs time and nothing else.

import init, { profiles } from '../photon/photon_wasm.js';

const ui = {
  modeCamera: document.getElementById('mode-camera'),
  modeFile: document.getElementById('mode-file'),
  cameraPane: document.getElementById('camera-pane'),
  filePane: document.getElementById('file-pane'),
  preview: document.getElementById('preview'),
  video: document.getElementById('video'),
  profile: document.getElementById('profile'),
  start: document.getElementById('start'),
  stop: document.getElementById('stop'),
  live: document.getElementById('live'),
  bar: document.getElementById('bar'),
  aim: document.getElementById('aim'),
  sample: document.getElementById('sample'),
  sampleRow: document.getElementById('sample-row'),
  status: document.getElementById('status'),
  result: document.getElementById('result'),
  download: document.getElementById('download'),
  player: document.getElementById('player'),
  grabber: document.getElementById('grabber'),
  facts: {
    blocks: document.getElementById('fact-blocks'),
    perCell: document.getElementById('fact-percell'),
    frames: document.getElementById('fact-frames'),
    used: document.getElementById('fact-used'),
    missed: document.getElementById('fact-missed'),
    straddled: document.getElementById('fact-straddled'),
    header: document.getElementById('fact-header'),
    cells: document.getElementById('fact-cells'),
    doubtful: document.getElementById('fact-doubtful'),
    name: document.getElementById('fact-name'),
  },
};

/** How much faster than real time to play a recorded file. */
const PLAYBACK_RATE = 4;

/**
 * Pixels per cell below which a capture is not going to work.
 *
 * Each cell carries a 4x4 shape mask, so at four pixels per cell a sub-cell is
 * a single pixel and the alphabet stops being separable. The measurement in
 * `docs/phase-1-report.md` is what this threshold comes from, and it is the one
 * failure no amount of extra filming repairs.
 */
const MINIMUM_PIXELS_PER_CELL = 5;

let PROFILES = [];
let worker = null;
let session = null;
let mode = 'camera';

function say(message, kind = '') {
  ui.status.textContent = message;
  ui.status.className = `status ${kind}`;
  ui.status.classList.remove('hidden');
}

function setMode(next) {
  mode = next;
  ui.cameraPane.classList.toggle('hidden', next !== 'camera');
  ui.filePane.classList.toggle('hidden', next !== 'file');
  ui.modeCamera.className = next === 'camera' ? '' : 'secondary';
  ui.modeFile.className = next === 'file' ? '' : 'secondary';
  ui.start.textContent = next === 'camera' ? 'Start the camera' : 'Read the recording';
  ui.start.disabled = next === 'file' && !ui.video.files?.length;
}

function reset() {
  ui.sampleRow.style.display = 'none';
  ui.result.classList.add('hidden');
  ui.download.classList.add('hidden');
  ui.status.classList.add('hidden');
  ui.aim.textContent = '';
  ui.bar.value = 0;
  for (const node of Object.values(ui.facts)) node.textContent = '—';
}

/** Starts reading, from whichever source is selected. */
async function start() {
  reset();
  ui.live.classList.remove('hidden');
  ui.start.disabled = true;
  ui.stop.classList.remove('hidden');

  session = {
    frames: 0,
    used: 0,
    missed: 0,
    straddled: 0,
    headerLost: 0,
    cellsLost: 0,
    sampled: false,
    doubtfulTotal: 0,
    doubtfulFrames: 0,
    perCell: null,
    diagnosis: null,
    busy: false,
    finished: false,
    url: null,
    stream: null,
  };

  worker = new Worker(new URL('./decoder-worker.js', import.meta.url), { type: 'module' });
  worker.onmessage = onWorkerMessage;
  // Index 0 is "detect automatically". The frames say which profile drew
  // them, so making a person match a setting on two devices only creates a way
  // to get it wrong.
  const chosen = ui.profile.selectedIndex === 0
    ? null
    : PROFILES[ui.profile.selectedIndex - 1].id;
  worker.postMessage({ type: 'start', profile: chosen });
}

function onWorkerMessage(event) {
  const message = event.data;

  switch (message.type) {
    case 'ready':
      if (mode === 'camera') openCamera();
      else playFile();
      break;

    case 'report':
      session.busy = false;
      applyReport(message.report);
      break;

    case 'done':
      finishWith(message.name, message.bytes);
      break;

    case 'failed':
      session.busy = false;
      concludeFailure(message.message);
      break;

    default:
      break;
  }
}

function applyReport(report) {
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
    // Kept apart because they mean opposite things. A header that will not
    // read is solid black-and-white cells failing, which points at geometry or
    // exposure. A payload that will not decode after a readable header points
    // at the cell classifier, and the doubtful rate below says which.
    case 'headerUnreadable':
      session.headerLost += 1;
      break;
    case 'payloadUnrecoverable':
      session.cellsLost += 1;
      break;
    case 'decoded':
      session.used += 1;
      break;
    default:
      break;
  }

  // The doubtful rate is measured on every frame that got as far as reading
  // cells, not only on the ones that worked. On a failing capture it is the
  // most informative number available: near zero means the cells are being read
  // confidently and wrongly, which is a different fault from reading them
  // uncertainly.
  if (report.outcome === 'decoded' || report.outcome === 'payloadUnrecoverable') {
    session.doubtfulTotal += report.doubtfulRate;
    session.doubtfulFrames += 1;
  }

  if (typeof report.pixelsPerCell === 'number') {
    session.perCell = report.pixelsPerCell;
    ui.facts.perCell.textContent = report.pixelsPerCell.toFixed(1);
  }
  if (report.diagnosis) {
    session.diagnosis = report.diagnosis;
  }

  ui.facts.used.textContent = String(session.used);
  ui.facts.missed.textContent = String(session.missed);
  ui.facts.straddled.textContent = String(session.straddled);
  ui.facts.header.textContent = String(session.headerLost);
  ui.facts.cells.textContent = String(session.cellsLost);

  if (report.needed > 0) {
    ui.bar.max = report.needed;
    ui.bar.value = Math.min(report.accepted, report.needed);
    ui.facts.blocks.textContent = `${report.accepted} of about ${report.needed}`;
  }

  if (session.doubtfulFrames > 0) {
    const rate = (session.doubtfulTotal / session.doubtfulFrames) * 100;
    ui.facts.doubtful.textContent = `${rate.toFixed(2)}%`;
  }

  updateAim();
}

/**
 * Says the one thing the person holding the camera can act on.
 *
 * Pixels per cell first, because it is the only failure extra filming does not
 * fix, and because it is the only one they can change by moving.
 */
function updateAim() {
  // Frames caught mid-change come first: it is the one failure that is entirely
  // the sending device's to fix, and no amount of aiming or waiting helps.
  if (session.straddled > 4 && session.straddled > session.used) {
    ui.aim.textContent =
      `${session.straddled} pictures caught two codes at once — the sending ` +
      'screen is changing faster than this camera can capture a whole one. ' +
      'Raise "hold each code" on the sending device. Nothing here will fix it.';
    return;
  }
  if (session.perCell !== null && session.perCell < MINIMUM_PIXELS_PER_CELL) {
    ui.aim.textContent =
      `Only ${session.perCell.toFixed(1)} camera pixels per cell — too few for the ` +
      `cells to be read reliably. Move closer, or fill more of the frame with the screen.`;
    return;
  }
  // A readable header and unreadable cells is its own fault, and the doubtful
  // rate distinguishes its two causes.
  if (session.cellsLost > 4 && session.cellsLost > session.used) {
    const rate = session.doubtfulFrames > 0
      ? (session.doubtfulTotal / session.doubtfulFrames) * 100
      : 0;
    ui.aim.textContent = rate > 2
      ? `The code is found and its header reads, but ${rate.toFixed(1)}% of cells ` +
        'are being read uncertainly. That is focus, glare or motion — steady the ' +
        'camera, get the whole screen evenly lit, and let it focus.'
      : 'The code is found and its header reads, but the cells decode to the ' +
        'wrong values with high confidence. Please save a picture below — this ' +
        'is not something the counters can explain.';
    return;
  }
  if (session.headerLost > 4 && session.headerLost > session.used) {
    ui.aim.textContent =
      'The code is found but its header will not read. The header is the ' +
      'sturdiest part of a frame, so this usually means the code is being ' +
      'scaled or clipped on the sending screen. Please save a picture below.';
    return;
  }
  if (session.frames > 12 && session.used === 0) {
    const found = session.diagnosis?.finderCandidates ?? 0;
    if (found === 0) {
      ui.aim.textContent =
        'No part of a code is visible yet. Point the camera at the sending ' +
        'screen, get closer, and keep the whole of it in shot.';
    } else if (!session.diagnosis?.quadFound) {
      ui.aim.textContent =
        `Seeing ${found} of the four corner patterns. Usually that means part ` +
        'of the code is out of shot or cut off at the edge of the sending ' +
        'screen — check the whole code is visible there, and get all of it in frame.';
    } else {
      ui.aim.textContent =
        'The corners are visible but the frame is not readable. Hold steadier, ' +
        'move any glare off the screen, and get closer.';
    }
    return;
  }
  if (session.used > 0) {
    ui.aim.textContent =
      'Reading. Keep the screen in shot — this stops and offers the file by ' +
      'itself as soon as it has enough, however many passes that takes.';
  }
}

/** Live capture. */
async function openCamera() {
  try {
    // Ask for as much resolution as the device will give. Browsers hand out
    // less than the camera app records at, and how much less is the difference
    // between this working and not, so it is worth asking loudly.
    session.stream = await navigator.mediaDevices.getUserMedia({
      video: {
        facingMode: { ideal: 'environment' },
        width: { ideal: 3840 },
        height: { ideal: 2160 },
        frameRate: { ideal: 30 },
      },
      audio: false,
    });
  } catch (error) {
    concludeFailure(`the camera could not be opened: ${error}`);
    return;
  }

  ui.preview.srcObject = session.stream;
  await ui.preview.play().catch(() => {});

  const track = session.stream.getVideoTracks()[0];
  const settings = track?.getSettings?.() ?? {};
  const width = settings.width ?? ui.preview.videoWidth;
  const height = settings.height ?? ui.preview.videoHeight;

  ui.grabber.width = width;
  ui.grabber.height = height;
  say(`Camera open at ${width}×${height}. Point it at the other screen.`);

  pump(ui.preview);
}

/** A recording already on the device. */
function playFile() {
  const file = ui.video.files?.[0];
  if (!file) {
    concludeFailure('no recording was chosen');
    return;
  }

  session.url = URL.createObjectURL(file);
  const player = ui.player;
  player.src = session.url;
  player.playbackRate = PLAYBACK_RATE;

  player.onloadedmetadata = () => {
    ui.grabber.width = player.videoWidth;
    ui.grabber.height = player.videoHeight;
    say(`Reading ${player.videoWidth}×${player.videoHeight}. This takes a while.`);
  };

  player.onended = () => {
    if (!session.finished) {
      say('The recording ended. Working out what was recovered…');
      worker.postMessage({ type: 'finish' });
    }
  };

  player.onerror = () => concludeFailure('the browser could not read this video file');

  pump(player);
  player.play().catch((error) => concludeFailure(`the recording would not play: ${error}`));
}

/** Feeds presented frames to the worker, from either source. */
function pump(element) {
  if ('requestVideoFrameCallback' in element) {
    const step = () => {
      if (session.finished) return;
      grab(element);
      element.requestVideoFrameCallback(step);
    };
    element.requestVideoFrameCallback(step);
    return;
  }

  // Older browsers present no per-frame callback. Sampling on a timer sees
  // fewer frames, which costs time rather than correctness.
  const timer = setInterval(() => {
    if (session.finished) {
      clearInterval(timer);
      return;
    }
    grab(element);
  }, 40);
}

/**
 * Sends the frame now on screen to the worker, unless it is still busy.
 *
 * Skipping rather than queueing is deliberate. A queue would grow without
 * bound, hold every frame's pixels in memory, and go on delivering stale frames
 * long after the file was already recoverable.
 */
function grab(element) {
  if (session.busy || session.finished) return;
  if (!ui.grabber.width || !ui.grabber.height) return;

  const context = ui.grabber.getContext('2d', { willReadFrequently: true });
  context.drawImage(element, 0, 0, ui.grabber.width, ui.grabber.height);

  const picture = context.getImageData(0, 0, ui.grabber.width, ui.grabber.height);
  const buffer = picture.data.buffer;

  session.busy = true;
  session.frames += 1;
  ui.facts.frames.textContent = String(session.frames);

  // Ask for a diagnosis every so often, but only while nothing has worked yet.
  const diagnose = session.used === 0 && session.frames % 15 === 0;

  // Keep one picture that failed, exactly as the decoder saw it. Guessing at a
  // capture from counters has a limit, and this is where it ends: the same
  // bytes can go through the command line, which prints every measurement.
  if (!session.sampled && session.used === 0 && session.frames === 25) {
    session.sampled = true;
    ui.grabber.toBlob((blob) => {
      if (!blob) return;
      ui.sample.href = URL.createObjectURL(blob);
      ui.sample.download = 'photon-unread-frame.png';
      ui.sample.textContent = 'Save a picture this could not read (for diagnosis)';
      ui.sampleRow.style.display = '';
    }, 'image/png');
  }

  worker.postMessage(
    {
      type: 'frame',
      buffer,
      width: ui.grabber.width,
      height: ui.grabber.height,
      diagnose,
    },
    [buffer],
  );
}

function finishWith(name, buffer) {
  session.finished = true;
  stopSources();

  const bytes = new Uint8Array(buffer);
  const blob = new Blob([bytes], { type: 'application/octet-stream' });

  ui.facts.name.textContent = `${name} (${bytes.length} bytes)`;
  ui.download.href = URL.createObjectURL(blob);
  ui.download.download = name || 'photon-received';
  ui.download.textContent = `Save ${name}`;
  ui.download.className = '';
  ui.result.classList.remove('hidden');
  ui.download.classList.remove('hidden');
  ui.aim.textContent = '';

  say('Recovered, and the digest matches. The file is exactly what was sent.', 'good');
  ui.start.disabled = false;
  ui.stop.classList.add('hidden');
}

function concludeFailure(message) {
  if (session) session.finished = true;
  stopSources();

  const advice =
    session?.perCell !== null && session?.perCell < MINIMUM_PIXELS_PER_CELL
      ? ` At ${session.perCell.toFixed(1)} pixels per cell the screen was too small in` +
        ' the shot for the cells to be read. Get closer, or record with the camera app' +
        ' and load the file instead.'
      : '';

  say(`${message}.${advice}`, 'bad');
  ui.start.disabled = false;
  ui.stop.classList.add('hidden');
}

function stopSources() {
  ui.player.pause();
  ui.player.onended = null;
  session?.stream?.getTracks?.().forEach((track) => track.stop());
  ui.preview.srcObject = null;
  if (session?.url) URL.revokeObjectURL(session.url);
  worker?.terminate();
  worker = null;
}

function stop() {
  if (!session || session.finished) return;
  say('Stopping. Working out what was recovered…');
  ui.player.pause();
  session.stream?.getTracks?.().forEach((track) => track.stop());
  worker?.postMessage({ type: 'finish' });
}

ui.modeCamera.addEventListener('click', () => setMode('camera'));
ui.modeFile.addEventListener('click', () => setMode('file'));
ui.video.addEventListener('change', () => {
  ui.start.disabled = !ui.video.files?.length;
});
ui.start.addEventListener('click', start);
ui.stop.addEventListener('click', stop);

try {
  await init();
  PROFILES = JSON.parse(profiles());

  const auto = document.createElement('option');
  auto.textContent = 'Detect automatically';
  ui.profile.append(auto);

  for (const profile of PROFILES) {
    const option = document.createElement('option');
    option.textContent = profile.name;
    ui.profile.append(option);
  }
  ui.profile.selectedIndex = 0;

  const hasCamera = Boolean(navigator.mediaDevices?.getUserMedia);
  setMode(hasCamera ? 'camera' : 'file');
  if (!hasCamera) {
    ui.modeCamera.disabled = true;
    ui.modeCamera.title = 'This browser does not offer camera access';
  }
} catch (error) {
  say(`The protocol module failed to load: ${error}`, 'bad');
  ui.start.disabled = true;
}
