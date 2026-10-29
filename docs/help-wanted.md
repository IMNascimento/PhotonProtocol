# Help wanted

Features the project wants and has not built. Each one is written so that
someone who has never opened this repository can pick it up without asking:
what the problem is, in numbers; what already exists and why it is not enough;
what would count as done; and where the code goes.

To take one, open an issue saying so, so two people do not build it twice. The
rules in [CONTRIBUTING.md](../CONTRIBUTING.md) apply, and two of them matter
here more than the rest: no change to the wire format without `SPEC.md` in the
same commit, and no claim of improvement without a measurement.

---

## 1. Shrink a photo before sending it

**Status:** open. **Touches:** the sending page, maybe `photon-wasm`. **Does
not touch:** the wire format.

### The problem

The channel is slow, and a photo is big. On the one real phone this has been
measured on, an iPhone filming a 1080p monitor, the only profile that decodes
is `P1-conservative`, at 23.7 KB/s with ten codes a second and 38.2 KB/s with
twenty ([docs/benchmarks](benchmarks/README.md)).

| What is sent | Size | At 23.7 KB/s | At 38.2 KB/s |
| --- | --- | --- | --- |
| A screenshot | 300 KB | 13 s | 8 s |
| A photo already resized for the web | 1 MB | 44 s | 27 s |
| A photo straight from a phone camera | 3 MB | 2 min 12 s | 1 min 20 s |

Nobody holds a phone still at a monitor for two minutes. A transfer of that
length does not fail; it is abandoned. The dense format on this branch promises
ten to twenty times more, but only through a simulated camera so far, and even
then a 3 MB photo would be the slow case.

### What exists already, and why it does not help

The sender already compresses. `photon_core::file::compress` runs Brotli at its
highest quality, and the manifest carries the algorithm and the original size
(`SPEC.md` §7.2). It is lossless, which is right for a document, a source tree
or a spreadsheet: those shrink by two thirds or more and arrive byte for byte.

A photo does not shrink. A JPEG, an HEIC, a PNG of a photograph, an MP4: these
are compressed already, and Brotli finds nothing in them. The code knows this
and declines: it probes three samples, sees no gain, and declares `0x00`,
*none*. Running Brotli harder would not change the answer. There is no
lossless trick left to play on a photo, and this feature is not about finding
one.

### What the feature is

Make the photo smaller before the protocol ever sees it, by re-encoding it:
fewer pixels, a lower JPEG quality, or a better codec, chosen so that the
result is as small as it can be while still being the picture the person meant
to send. A 12-megapixel, 3 MB photo re-encoded at 1600 pixels on its long side
and JPEG quality 80 is around 250 KB, which is ten seconds at 23.7 KB/s rather
than two minutes. Most people sending a photo across a room want the picture,
not the pixels.

That is a product decision as much as an engineering one, which is why it has
to be visible and chosen, never silent:

- The sender detects that the file is an image (by content, not by extension)
  and offers to shrink it, saying what it would cost: *3.1 MB → about 250 KB,
  2 min → 10 s*. The numbers come from actually re-encoding, not from a guess.
- The person picks. A sensible default is preselected (a long side of 1600 or
  2048 px, quality around 80), with at least "original" and one or two smaller
  steps on offer. The estimate line the page already shows (`refreshEstimate`
  in `web-emitter/emitter.js`) updates to the chosen size.
- The receiver gets a complete, ordinary file: a smaller JPEG with the same
  name, or a name that says it was reduced. It needs no new code. Nothing in
  the manifest changes.
- Anything that is not an image is left alone, exactly as today.

### Constraints that shape the design

- **It runs in a browser on whatever device is sending.** Often a phone. The
  page is static and has no server to lean on: the whole site is files served
  from GitHub Pages, and nothing leaves the device. Re-encoding has to happen
  in the page, in JavaScript or WebAssembly.
- **The browser can already decode and encode images.** `createImageBitmap`,
  an `OffscreenCanvas` and `canvas.convertToBlob({ type, quality })` re-encode
  a JPEG or WebP without any library, and respect the EXIF orientation when
  asked (`imageOrientation: 'from-image'`). That is probably the whole
  implementation, and it keeps the WebAssembly bundle small. A Rust encoder in
  `photon-wasm` is the fallback if browser encoding proves too inconsistent to
  estimate with, not the first choice.
- **HEIC is the common case on an iPhone and most browsers cannot decode it.**
  Safari can; Chrome and Firefox cannot. Decide what happens there: ask iOS for
  a JPEG in the first place (`accept="image/*"` on the file input often does
  this), decode HEIC in WebAssembly, or say plainly that this file cannot be
  shrunk here. Saying so is acceptable; silently sending 4 MB is not.
- **Be honest about the estimate.** The page says how long a transfer will
  take before it starts. If re-encoding changes the size, the estimate must
  follow it, and it must be the size of the file that will actually be sent.
- **Do not freeze the page.** Decoding a 12-megapixel image and encoding it
  again takes a noticeable fraction of a second on a phone. Do it off the main
  thread (a worker, or at least `await` the async canvas API) and show that it
  is happening.
- **Metadata is a decision, not an accident.** Re-encoding through a canvas
  drops EXIF, including the GPS position. That is probably what a person
  sending a photo to a stranger's screen wants, and it should be said in the
  interface rather than discovered later.

### Where it goes

- `web-emitter/emitter.js`: the file is read in `start()` with
  `file.arrayBuffer()` and handed to `Emitter` or `DenseEmitter` as bytes.
  The shrinking step sits between the file input and that call, and the
  estimate in `refreshEstimate()` has to see its result.
- `web-emitter/index.html` and the `strings` table at the top of `emitter.js`:
  the controls and their text, in English and Portuguese both (every string on
  the page is in `{ en, pt }` form already).
- `photon_core::file` and `SPEC.md`: **nothing.** The protocol receives a file
  and sends it. Which file is the sender's business. If you find yourself
  wanting a manifest flag that says "this was reduced", resist it: a reduced
  JPEG is a JPEG, and the receiver does not need to know.
- `tools/e2e/send.mjs` photographs the real sending page in Chrome; extend it
  so a photo passes through the shrinking step and the decoded result is
  checked for size and for being a valid image.

### What done looks like

1. On the sending page, choosing a 3 MB camera photo offers a reduced version
   with a real size and a real time, in under a second on a mid-range phone,
   without freezing the page.
2. Choosing it sends the reduced file. The receiver downloads a valid JPEG (or
   WebP) of the chosen dimensions, with a sensible name.
3. Choosing "original" sends the bytes untouched, as today, hash and all.
4. A PDF, a ZIP, a text file: no offer, no change in behaviour.
5. An HEIC on a browser that cannot decode it: a clear message, not a silent
   failure and not a silent 4 MB transfer.
6. A measurement in the pull request: for three photos of different sizes, the
   original size, the reduced size at the default setting, the time to
   re-encode on a named phone, and the transfer time before and after on the
   real phone or, failing that, through `tools/e2e/send.mjs`.
7. Tests in `tools/e2e` that fail if the shrinking step produces something the
   decoder cannot read, or produces nothing.

### Open questions, for the issue thread

- What is the right default? 1600 px at quality 80 is a guess. A long side of
  2048 keeps a photo usable on a monitor; 1200 is enough for a chat message.
  Measure a few and say which and why.
- JPEG or WebP? WebP is around a quarter smaller at the same quality and every
  current browser encodes it, but not every program the receiver might open it
  with reads it. JPEG is the safe default; WebP could be an option.
- Should the receiver's name say it was reduced (`photo.jpg` →
  `photo-1600.jpg`)? Probably yes, so nobody mistakes it for the original.
- Video is the same problem ten times over, and is out of scope here. If the
  browser-side approach to photos works, a follow-up entry for video can say
  what it would take.

---

## Adding an entry

An entry is worth adding when it has all of: the problem in numbers, what
already exists and why it falls short, what done looks like as a checklist a
reviewer can tick, and where in the code it goes. A wish without those is an
issue, not an entry; open one and we will turn it into an entry together.
