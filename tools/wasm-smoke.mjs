// Drives the WebAssembly bindings from JavaScript, the way the pages do.
//
// The Rust tests exercise the binding functions as Rust. That leaves a gap
// exactly where the interesting mistakes live: a buffer sized wrongly across
// the boundary, an argument order that only matters from the other side, a
// return value the glue code copies rather than transfers. This runs the same
// round trip through the generated JavaScript, so the gap is covered by
// something a browser would agree with.
//
//   wasm-pack build wasm --release --target nodejs --out-dir pkg-node
//   node tools/wasm-smoke.mjs

import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { existsSync } from 'node:fs';

const require = createRequire(import.meta.url);
const packagePath = resolve(import.meta.dirname, '..', 'wasm', 'pkg-node', 'photon_wasm.js');

if (!existsSync(packagePath)) {
  console.error('build the Node package first:');
  console.error('  wasm-pack build wasm --release --target nodejs --out-dir pkg-node');
  process.exit(1);
}

const photon = require(packagePath);

let failures = 0;

function check(condition, description) {
  if (condition) {
    console.log(`  ok   ${description}`);
  } else {
    console.error(`  FAIL ${description}`);
    failures += 1;
  }
}

console.log('versions and profiles');
check(photon.protocolVersion() === 1, 'protocol version is 1');
check(typeof photon.specVersion() === 'string', 'specification version is a string');

const profiles = JSON.parse(photon.profiles());
check(Array.isArray(profiles) && profiles.length === 3, 'three profiles are described');
check(
  profiles.every((p) => p.grid > 0 && p.payloadCapacity > 0 && p.bitsPerCell >= 4),
  'every profile carries usable geometry',
);

console.log('round trip through the bindings');

// Incompressible, so the transfer really has to span several frames. Text
// compresses hard enough that a nominally large file arrives in one, which
// would leave the multi-frame path — where the fountain code actually does
// something — untested.
const source = Buffer.alloc(20000);
let state = 0x2545f491;
for (let i = 0; i < source.length; i += 1) {
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  source[i] = state & 0xff;
}

const profile = profiles[1];
const emitter = new photon.Emitter('smoke.txt', source, profile.id, 8, 0x0badc0de);
const side = emitter.side();
const manifest = JSON.parse(emitter.manifest());

check(manifest.name === 'smoke.txt', 'the manifest carries the file name');
check(manifest.originalSize === source.length, 'the manifest carries the original size');
check(side > 0, 'the emitter reports a frame size');

const receiver = new photon.Receiver(profile.id);
let frames = 0;
let decoded = 0;

while (!receiver.isComplete() && frames < 60) {
  const rgba = emitter.nextFrame();
  if (frames === 0) {
    check(rgba.length === side * side * 4, 'a frame is RGBA of the stated size');
  }
  const report = JSON.parse(receiver.acceptFrame(rgba, side, side));
  if (report.outcome === 'decoded') decoded += 1;
  frames += 1;
}

check(receiver.isComplete(), `the transfer completed in ${frames} frames`);
check(frames > 1, 'the transfer spanned more than one frame');
check(decoded === frames, 'every frame decoded');
check(receiver.fileName() === 'smoke.txt', 'the receiver learned the file name');

const recovered = Buffer.from(receiver.finish());
check(recovered.equals(source), 'the recovered bytes are the bytes that went in');

console.log('rejecting bad input');
let refused = false;
try {
  new photon.Receiver(0x7f);
} catch {
  refused = true;
}
check(refused, 'an unknown profile is refused');

refused = false;
try {
  const spare = new photon.Receiver(profile.id);
  spare.acceptFrame(new Uint8Array(16), 100, 100);
} catch {
  refused = true;
}
check(refused, 'a mis-sized frame buffer is refused');

if (failures > 0) {
  console.error(`\n${failures} check(s) failed`);
  process.exit(1);
}
console.log('\nall checks passed');
