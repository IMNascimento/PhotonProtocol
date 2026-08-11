// Where the decoding actually happens.
//
// Reading a frame means locating the code, classifying every cell and running
// error correction over the result — hundreds of milliseconds of arithmetic. On
// the main thread that is a page which stops responding; here it is a page that
// keeps drawing progress while the work goes on.

import init, { Receiver } from '../photon/photon_wasm.js';

let receiver = null;

self.onmessage = async (event) => {
  const message = event.data;

  try {
    switch (message.type) {
      case 'start': {
        await init();
        // `undefined` asks the receiver to work the profile out from the
        // frames, which is what it does unless someone insists otherwise.
        receiver = new Receiver(message.profile ?? undefined);
        self.postMessage({ type: 'ready' });
        break;
      }

      case 'frame': {
        if (!receiver) return;
        const rgba = new Uint8Array(message.buffer);
        const report = JSON.parse(receiver.acceptFrame(rgba, message.width, message.height));
        self.postMessage({ type: 'report', report, index: message.index });

        if (report.complete) {
          const bytes = receiver.finish();
          self.postMessage(
            { type: 'done', name: receiver.fileName(), bytes: bytes.buffer },
            [bytes.buffer],
          );
          receiver = null;
        }
        break;
      }

      case 'finish': {
        if (!receiver) return;
        // Asked to stop early, or the recording ran out. Whatever the receiver
        // has is what it has, and the protocol's own message says which stage
        // it fell short at and by how much.
        try {
          const bytes = receiver.finish();
          self.postMessage(
            { type: 'done', name: receiver.fileName(), bytes: bytes.buffer },
            [bytes.buffer],
          );
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
