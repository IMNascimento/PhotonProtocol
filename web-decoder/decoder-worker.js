// Where the decoding actually happens.
//
// Reading a frame means locating the code, classifying every cell and running
// error correction over the result — tens of milliseconds of arithmetic on a
// desktop and several times that on a phone. On the main thread that is a page
// which stops responding; here it is a page that keeps drawing progress while
// the work goes on.
//
// The same file runs in two roles, chosen by the message that starts it.
//
// A *reader* is given pictures and keeps nothing. Reading a picture depends on
// no other picture, so the page runs several readers side by side and a phone's
// cores are all put to work.
//
// The *collector* is given what the readers read and never sees a picture. It
// holds the transfer: which codes have arrived, which blocks they carried, and
// in the end the file.

import init, {
  DenseReader,
  DenseReceiver,
  Reader,
  Receiver,
  inspectFrame,
} from '../photon/photon_wasm.js';

// There are two formats, a code of cells and a dense code, and a picture says
// which it is of. So a reader has a reader of each and the collector a receiver
// of each, and whichever the pictures turn out to be for does the work.
let reader = null;
let denseReader = null;
let receiver = null;
let denseReceiver = null;
let described = false;
let slowdown = 1;
let canvas = null;

/** Spends time doing nothing, to stand in for a slower device. */
function idle(milliseconds) {
  const until = performance.now() + milliseconds;
  while (performance.now() < until) {
    // Deliberately busy: a timer would let the next picture in.
  }
}

/** Hands the finished file to the page. */
function deliver(from) {
  const name = from.fileName();
  const bytes = from.finish();
  self.postMessage({ type: 'done', name, bytes: bytes.buffer }, [bytes.buffer]);
  receiver = null;
  denseReceiver = null;
}

/**
 * The pixels of a picture that arrived as a bitmap.
 *
 * Copying a picture out of a video is a third of the work of reading it, and
 * done by the page it is done by the one thread that also has to draw. A bitmap
 * can be handed over without being copied, and copied out here.
 */
function pixelsOf(message) {
  if (!message.bitmap) return new Uint8Array(message.buffer);

  const { width, height } = message;
  if (!canvas || canvas.width !== width || canvas.height !== height) {
    canvas = new OffscreenCanvas(width, height);
  }
  const context = canvas.getContext('2d', { willReadFrequently: true });
  context.drawImage(message.bitmap, 0, 0);
  message.bitmap.close();
  return new Uint8Array(context.getImageData(0, 0, width, height).data.buffer);
}

/** Reads one picture as a dense code. Says whether there was one in it. */
function readDense(message, rgba, began) {
  const packed = denseReader.read(rgba, message.width, message.height);
  const summary = JSON.parse(denseReader.summary());
  if (!summary.located) return false;

  summary.kind = 'dense';
  summary.outcome = 'opened';
  if (slowdown > 1) idle((performance.now() - began) * (slowdown - 1));
  summary.decodeMs = performance.now() - began;

  const reading = packed.buffer;
  self.postMessage(
    { type: 'read', id: message.id, summary, reading, dense: true, region: message.region },
    [reading],
  );
  return true;
}

/** Reads one picture, as far as it is worth reading. */
function readPicture(message) {
  const began = performance.now();
  const rgba = pixelsOf(message);

  if (message.kind !== 'cells') {
    // Reading takes the pixels with it, so while it is not known which format
    // the pictures are of, a copy is kept for the other.
    const spare = message.kind === 'dense' ? null : rgba.slice();
    if (readDense(message, rgba, began)) return;
    if (!spare) {
      const summary = { kind: 'dense', outcome: 'notLocated' };
      summary.decodeMs = performance.now() - began;
      self.postMessage({ type: 'read', id: message.id, summary, region: message.region });
      return;
    }
    readCells(message, spare, began);
    return;
  }
  readCells(message, rgba, began);
}

/** Reads one picture as a code of cells. */
function readCells(message, rgba, began) {
  const summary = JSON.parse(reader.open(rgba, message.width, message.height));
  let reading = null;

  if (summary.outcome === 'opened') {
    if (message.session !== null && summary.session !== message.session) {
      // Somebody else's transfer, in shot by accident.
      summary.outcome = 'wrongSession';
      reader.release();
    } else if (message.taken.includes(summary.sequence)) {
      // A camera sees most codes more than once. The header says which code
      // this is before any of the payload has been paid for.
      summary.outcome = 'duplicate';
      reader.release();
    } else {
      // Say which code is being read before reading it, so that the next
      // picture of the same code is not read as well.
      self.postMessage({ type: 'opened', id: message.id, sequence: summary.sequence });
      reading = reader.read().buffer;
    }
  } else if (message.diagnose && summary.outcome === 'notLocated') {
    // When nothing is being found, occasionally ask why. It costs another
    // detection pass, so it is only worth doing while the answer would change
    // what the person watching should do.
    summary.diagnosis = JSON.parse(inspectFrame(rgba, message.width, message.height));
  }

  if (slowdown > 1) idle((performance.now() - began) * (slowdown - 1));
  summary.decodeMs = performance.now() - began;

  self.postMessage(
    { type: 'read', id: message.id, summary, reading, region: message.region },
    reading ? [reading] : [],
  );
}

/** Folds one reading into the transfer. */
function absorb(message) {
  const into = message.dense ? denseReceiver : receiver;
  const began = performance.now();
  const report = JSON.parse(into.absorb(new Uint8Array(message.reading)));
  report.absorbMs = performance.now() - began;

  // The name and size arrive with the first manifest, and the page wants them
  // once.
  if (!described && report.needed > 0) {
    report.name = into.fileName();
    report.size = into.fileSize();
    described = true;
  }

  self.postMessage({ type: 'report', id: message.id, report });
  if (report.complete) deliver(into);
}

self.onmessage = async (event) => {
  const message = event.data;

  try {
    switch (message.type) {
      case 'start': {
        await init();
        slowdown = message.slowdown ?? 1;
        if (message.role === 'collector') {
          // No profile is given, so the receiver works it out from the
          // frames, which say which profile drew them.
          receiver = new Receiver(undefined);
          denseReceiver = new DenseReceiver();
          described = false;
        } else {
          reader = new Reader();
          denseReader = new DenseReader();
        }
        self.postMessage({ type: 'ready' });
        break;
      }

      case 'frame':
        if (reader) readPicture(message);
        break;

      case 'absorb':
        if (receiver) absorb(message);
        break;

      case 'finish': {
        if (!receiver) return;
        // Asked to stop early, or the recording ran out. Whatever the receiver
        // has is what it has, and the protocol's own message says which stage
        // it fell short at and by how much.
        try {
          // Whichever of the two has been told what file this is.
          deliver(denseReceiver.fileName() === undefined ? receiver : denseReceiver);
        } catch (error) {
          self.postMessage({ type: 'failed', message: String(error) });
        }
        receiver = null;
        break;
      }

      default:
        break;
    }
  } catch (error) {
    self.postMessage({ type: 'failed', message: String(error) });
  }
};
