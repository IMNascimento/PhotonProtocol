// Saves pictures exactly as a page receives them from the camera.
//
//   node tools/e2e/grab.mjs <camera.y4m> <outputDir> [count]
//
// For checking the harness rather than the protocol: if what the browser hands
// a page differs from what `photon film` wrote, every conclusion drawn from the
// end-to-end run is about the difference and not about the decoder.

import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import puppeteer from 'puppeteer-core';

const [recording, out, count = '3'] = process.argv.slice(2);
if (!recording || !out) {
  console.error('usage: node tools/e2e/grab.mjs <camera.y4m> <outputDir> [count]');
  process.exit(2);
}

const executablePath = [process.env.CHROME, '/usr/bin/google-chrome', '/usr/bin/chromium']
  .find((path) => path && existsSync(path));

const server = createServer((request, response) => {
  response.writeHead(200, { 'content-type': 'text/html' });
  response.end('<video id="v" playsinline muted autoplay></video><canvas id="c"></canvas>');
});
await new Promise((done) => server.listen(0, '127.0.0.1', done));

const browser = await puppeteer.launch({
  executablePath,
  headless: true,
  args: [
    '--use-fake-ui-for-media-stream',
    '--use-fake-device-for-media-stream',
    `--use-file-for-fake-video-capture=${resolve(recording)}`,
    '--autoplay-policy=no-user-gesture-required',
  ],
});

try {
  const page = await browser.newPage();
  // The camera is only offered to a secure context, which a page with no
  // origin is not. `127.0.0.1` over plain HTTP is.
  await page.goto(`http://127.0.0.1:${server.address().port}/`, { waitUntil: 'domcontentloaded' });

  const pictures = await page.evaluate(async (wanted) => {
    const stream = await navigator.mediaDevices.getUserMedia({
      video: { width: { ideal: 3840 }, height: { ideal: 2160 }, frameRate: { ideal: 30 } },
      audio: false,
    });
    const video = document.getElementById('v');
    video.srcObject = stream;
    await video.play();

    const settings = stream.getVideoTracks()[0].getSettings();
    const canvas = document.getElementById('c');
    canvas.width = video.videoWidth;
    canvas.height = video.videoHeight;
    const context = canvas.getContext('2d', { willReadFrequently: true });

    const out = [];
    for (let index = 0; index < wanted; index += 1) {
      await new Promise((done) => video.requestVideoFrameCallback(done));
      context.drawImage(video, 0, 0);
      out.push(canvas.toDataURL('image/png'));
    }
    return { settings, width: video.videoWidth, height: video.videoHeight, out };
  }, Number(count));

  console.log(`track settings ${JSON.stringify(pictures.settings)}`);
  console.log(`video element  ${pictures.width}x${pictures.height}`);

  await mkdir(out, { recursive: true });
  for (const [index, url] of pictures.out.entries()) {
    const path = join(out, `grab-${String(index).padStart(3, '0')}.png`);
    await writeFile(path, Buffer.from(url.split(',')[1], 'base64'));
    console.log(`wrote ${path}`);
  }
} finally {
  await browser.close();
  server.close();
}
