// The receiving half. Plays the recording, hands frames to a worker, and shows
// what the protocol makes of them.
//
// Frames are taken from a `<video>` element rather than demuxed by hand. That
// means the browser's own hardware decoder does the container and codec work,
// which no amount of JavaScript would do better, and it keeps this page free of
// the megabyte of demuxer a WebCodecs pipeline would need.
//
// The consequence is that frames arrive as fast as they play and are dropped
// when the worker is busy. For most formats that would be a problem. Here it is
// the assumption the protocol was built on: any large enough subset of the
// stream rebuilds the file, so a skipped frame costs time and nothing else.

import init, { profiles } from '../photon/photon_wasm.js';

const ui = {
  video: document.getElementById('video'),
  profile: document.getElementById('profile'),
  start: document.getElementById('start'),
  stop: document.getElementById('stop'),
  live: document.getElementById('live'),
  bar: document.getElementById('bar'),
  status: document.getElementById('status'),
  result: document.getElementById('result'),
  download: document.getElementById('download'),
  player: document.getElementById('player'),
  grabber: document.getElementById('grabber'),
  facts: {
    blocks: document.getElementById('fact-blocks'),
    frames: document.getElementById('fact-frames'),
    used: document.getElementById('fact-used'),
    missed: document.getElementById('fact-missed'),
    doubtful: document.getElementById('fact-doubtful'),
    name: document.getElementById('fact-name'),
  },
};

/** How much faster than real time to play the recording. */
const PLAYBACK_RATE = 4;

let PROFILES = [];
let worker = null;
let session = null;

function say(message, kind = '') {
  ui.status.textContent = message;
  ui.status.className = `status ${kind}`;
  ui.status.classList.remove('hidden');
}

function reset() {
  ui.result.classList.add('hidden');
  ui.download.classList.add('hidden');
  ui.status.classList.add('hidden');
  ui.bar.value = 0;
  for (const node of Object.values(ui.facts)) node.textContent = '—';
}

/** Starts reading the chosen recording. */
async function start() {
  const file = ui.video.files?.[0];
  if (!file) return;

  reset();
  ui.live.classList.remove('hidden');
  ui.start.disabled = true;
  ui.stop.classList.remove('hidden');
  say('Loading the recording…');

  session = {
    frames: 0,
    used: 0,
    missed: 0,
    doubtfulTotal: 0,
    doubtfulFrames: 0,
    busy: false,
    finished: false,
    url: URL.createObjectURL(file),
  };

  worker = new Worker(new URL('./decoder-worker.js', import.meta.url), { type: 'module' });
  worker.onmessage = onWorkerMessage;

  const profile = PROFILES[ui.profile.selectedIndex];
  worker.postMessage({ type: 'start', profile: profile.id });
}

function onWorkerMessage(event) {
  const message = event.data;

  switch (message.type) {
    case 'ready':
      play();
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
  if (report.outcome === 'notLocated') {
    session.missed += 1;
  } else if (report.outcome === 'decoded') {
    session.used += 1;
    session.doubtfulTotal += report.doubtfulRate;
    session.doubtfulFrames += 1;
  }

  ui.facts.used.textContent = String(session.used);
  ui.facts.missed.textContent = String(session.missed);

  if (report.needed > 0) {
    ui.bar.max = report.needed;
    ui.bar.value = Math.min(report.accepted, report.needed);
    ui.facts.blocks.textContent = `${report.accepted} of about ${report.needed}`;
  }

  if (session.doubtfulFrames > 0) {
    const rate = (session.doubtfulTotal / session.doubtfulFrames) * 100;
    ui.facts.doubtful.textContent = `${rate.toFixed(2)}%`;
  }
}

/** Plays the recording, feeding presented frames to the worker. */
function play() {
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

  if ('requestVideoFrameCallback' in player) {
    const pump = () => {
      if (session.finished) return;
      grab();
      player.requestVideoFrameCallback(pump);
    };
    player.requestVideoFrameCallback(pump);
  } else {
    // Older browsers present no per-frame callback. Sampling on a timer sees
    // fewer frames, which costs time rather than correctness.
    const timer = setInterval(() => {
      if (session.finished || player.ended) {
        clearInterval(timer);
        return;
      }
      grab();
    }, 40);
  }

  player.play().catch((error) => concludeFailure(`the recording would not play: ${error}`));
}

/**
 * Sends the frame now on screen to the worker, unless it is still busy.
 *
 * Skipping rather than queueing is deliberate. A queue would grow without
 * bound, hold every frame's pixels in memory, and deliver stale frames long
 * after the file was already recoverable.
 */
function grab() {
  if (session.busy || session.finished) return;

  const context = ui.grabber.getContext('2d', { willReadFrequently: true });
  context.drawImage(ui.player, 0, 0);

  const picture = context.getImageData(0, 0, ui.grabber.width, ui.grabber.height);
  const buffer = picture.data.buffer;

  session.busy = true;
  session.frames += 1;
  ui.facts.frames.textContent = String(session.frames);

  worker.postMessage(
    {
      type: 'frame',
      buffer,
      width: ui.grabber.width,
      height: ui.grabber.height,
      index: session.frames,
    },
    [buffer],
  );
}

function finishWith(name, buffer) {
  session.finished = true;
  stopPlayback();

  const bytes = new Uint8Array(buffer);
  const blob = new Blob([bytes], { type: 'application/octet-stream' });

  ui.facts.name.textContent = `${name} (${bytes.length} bytes)`;
  ui.download.href = URL.createObjectURL(blob);
  ui.download.download = name || 'photon-received';
  ui.download.textContent = `Save ${name}`;
  ui.download.className = '';
  ui.result.classList.remove('hidden');
  ui.download.classList.remove('hidden');

  say('Recovered, and the digest matches. The file is exactly what was sent.', 'good');
  ui.start.disabled = false;
  ui.stop.classList.add('hidden');
}

function concludeFailure(message) {
  session.finished = true;
  stopPlayback();
  say(message, 'bad');
  ui.start.disabled = false;
  ui.stop.classList.add('hidden');
}

function stopPlayback() {
  ui.player.pause();
  ui.player.onended = null;
  if (session?.url) URL.revokeObjectURL(session.url);
  worker?.terminate();
  worker = null;
}

function stop() {
  if (!session || session.finished) return;
  say('Stopping. Working out what was recovered…');
  ui.player.pause();
  worker?.postMessage({ type: 'finish' });
}

ui.video.addEventListener('change', () => {
  ui.start.disabled = !ui.video.files?.length;
});
ui.start.addEventListener('click', start);
ui.stop.addEventListener('click', stop);

try {
  await init();
  PROFILES = JSON.parse(profiles());

  for (const profile of PROFILES) {
    const option = document.createElement('option');
    option.textContent = profile.name;
    ui.profile.append(option);
  }
  ui.profile.selectedIndex = Math.min(1, PROFILES.length - 1);
} catch (error) {
  say(`The protocol module failed to load: ${error}`, 'bad');
  ui.start.disabled = true;
}
