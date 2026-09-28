// Runs the sending page in a real browser and photographs what it paints.
//
//   node tools/e2e/send.mjs <file> <outputDir> [options]
//
//   --screen WxH    the window the page is given (default 1920x1080)
//   --ratio N       device pixels per CSS pixel (default 1)
//   --rate N        codes per second to ask for (default 10)
//   --shots N       screenshots to take (default 12)
//   --profile N     index of the profile to choose (default: the page's own)
//
// The screenshots are what reaches the monitor, pixel for pixel. Decoding them
// with `photon decode <outputDir> --truth <file>` must find every cell of every
// one correct: there is no camera in this path, so anything wrong was drawn
// wrong. A canvas the layout quietly scaled, a cell size that does not divide,
// a code clipped by the window — all of them look fine to a person and all of
// them show up here.

import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { extname, join, normalize, resolve } from 'node:path';
import puppeteer from 'puppeteer-core';

const root = resolve(import.meta.dirname, '..', '..');
const site = join(root, 'site');

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
};

const positional = [];
const options = { screen: '1920x1080', ratio: 1, rate: 10, shots: 12, profile: null };
const argv = process.argv.slice(2);
for (let index = 0; index < argv.length; index += 1) {
  const argument = argv[index];
  if (argument === '--screen') options.screen = argv[++index];
  else if (argument === '--ratio') options.ratio = Number(argv[++index]);
  else if (argument === '--rate') options.rate = Number(argv[++index]);
  else if (argument === '--shots') options.shots = Number(argv[++index]);
  else if (argument === '--profile') options.profile = Number(argv[++index]);
  else positional.push(argument);
}

if (positional.length < 2) {
  console.error('usage: node tools/e2e/send.mjs <file> <outputDir> [--screen WxH] [--ratio N] [--rate N]');
  process.exit(2);
}

const file = resolve(positional[0]);
const out = resolve(positional[1]);
const [width, height] = options.screen.split('x').map(Number);

const executablePath = [process.env.CHROME, '/usr/bin/google-chrome', '/usr/bin/chromium']
  .find((path) => path && existsSync(path));
if (!executablePath) {
  console.error('no Chrome found; set CHROME to its path');
  process.exit(2);
}

const server = createServer(async (request, response) => {
  const url = new URL(request.url, 'http://localhost');
  let path = decodeURIComponent(url.pathname);
  if (path.endsWith('/')) path += 'index.html';
  const target = join(site, normalize(path).replace(/^(\.\.[/\\])+/, ''));
  try {
    const body = await readFile(target);
    response.writeHead(200, {
      'content-type': TYPES[extname(target)] ?? 'application/octet-stream',
      'cache-control': 'no-store',
    });
    response.end(body);
  } catch {
    response.writeHead(404).end('not found');
  }
});
await new Promise((done) => server.listen(0, '127.0.0.1', done));

const browser = await puppeteer.launch({
  executablePath,
  headless: true,
  args: ['--no-first-run', `--window-size=${width},${height}`],
});

let verdict = 1;

try {
  const page = await browser.newPage();
  await page.setViewport({ width, height, deviceScaleFactor: options.ratio });
  page.on('pageerror', (error) => console.log(`  page error: ${error.message}`));

  await page.goto(`http://127.0.0.1:${server.address().port}/emit/`, { waitUntil: 'networkidle0' });
  await page.waitForFunction(() => document.getElementById('profile').options.length > 0);

  const input = await page.$('#file');
  await input.uploadFile(file);

  await page.evaluate(
    (rate, profile) => {
      const slider = document.getElementById('rate');
      slider.value = String(rate);
      slider.dispatchEvent(new Event('input'));
      if (profile !== null) {
        const select = document.getElementById('profile');
        select.selectedIndex = profile;
        select.dispatchEvent(new Event('change'));
      }
    },
    options.rate,
    options.profile,
  );

  const chosen = await page.evaluate(() => {
    const select = document.getElementById('profile');
    return select.options[select.selectedIndex].textContent;
  });
  console.log(`profile         ${chosen}`);

  const clicked = Date.now();
  await page.click('#start');
  await page.waitForFunction(() => (window.photonStats?.frames ?? 0) > 0, { timeout: 120000 });
  console.log(`first code      ${Date.now() - clicked} ms after Start`);
  await page.waitForFunction(() => (window.photonStats?.frames ?? 0) > 2, { timeout: 30000 });

  await mkdir(out, { recursive: true });
  for (let index = 0; index < options.shots; index += 1) {
    const path = join(out, `screen-${String(index).padStart(3, '0')}.png`);
    await writeFile(path, await page.screenshot({ type: 'png' }));
    await new Promise((done) => setTimeout(done, 130));
  }

  // Long enough for the rate to mean something.
  await new Promise((done) => setTimeout(done, 3000));
  const stats = await page.evaluate(() => window.photonStats);
  const status = await page.evaluate(() => document.getElementById('status').textContent);

  console.log(`code            ${stats.side}x${stats.side} px, ${stats.cellPx} px a cell`);
  console.log(`display         ${stats.refresh} Hz, as the page measured it`);
  console.log(`codes           ${stats.perSecond} a second, asked for ${options.rate}`);
  console.log(`painting        ${stats.paintMs} ms a code`);
  if (status) console.log(`status          ${status}`);
  console.log(`screenshots     ${options.shots} in ${out}`);

  const near = Math.abs(stats.perSecond - options.rate) <= Math.max(1, options.rate * 0.15);
  if (!near) console.log('FAILED  the page is not painting at the rate it was asked for');
  else verdict = 0;
} finally {
  await browser.close();
  server.close();
}

process.exit(verdict);
