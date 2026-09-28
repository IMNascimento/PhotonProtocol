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

import init, { Reader, Receiver, inspectFrame } from '../photon/photon_wasm.js';

let reader = null;
let receiver = null;
let described = false;
let slowdown = 1;

/** Spends time doing nothing, to stand in for a slower device. */
function idle(milliseconds) {
  const until = performance.now() + milliseconds;
  while (performance.now() < until) {
    // Deliberately busy: a timer would let the next picture in.
  }
}

/** Hands the finished file to the page. */
function deliver() {
  const name = receiver.fileName();
  const bytes = receiver.finish();
  self.postMessage({ type: 'done', name, bytes: bytes.buffer }, [bytes.buffer]);
  receiver = null;
}

/** Reads one picture, as far as it is worth reading. */
function readPicture(message) {
  const began = performance.now();
  const rgba = new Uint8Array(message.buffer);
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
  const report = JSON.parse(receiver.absorb(new Uint8Array(message.reading)));

  // The name and size arrive with the first manifest, and the page wants them
  // once.
  if (!described && report.needed > 0) {
    report.name = receiver.fileName();
    report.size = receiver.fileSize();
    described = true;
  }

  self.postMessage({ type: 'report', id: message.id, report });
  if (report.complete) deliver();
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
          described = false;
        } else {
          reader = new Reader();
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
          deliver();
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
