// Runs the receiving page in a real browser, with a recording standing in for
// the camera, and checks that the file it offers is the file that was sent.
//
//   node tools/e2e/receive.mjs <camera.y4m> <original-file> [options]
//
//   --throttle N    slow the browser's CPU N times, to stand in for a phone
//   --timeout S     give up after S seconds (default 120)
//   --show          run with a window instead of headless
//
// Everything a unit test cannot reach is on this path: `getUserMedia`, the
// video element, the canvas read-back, the worker, the WebAssembly boundary and
// the page's own bookkeeping. A decoder that is correct and a page that never
// finishes are indistinguishable to the person holding the phone.
//
// The recording comes from `photon film`, which writes the pictures a phone's
// camera would have produced. Chrome plays it as the camera when started with
// `--use-file-for-fake-video-capture`, looping when it reaches the end.

import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
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

function parseArguments(argv) {
  const positional = [];
  const options = { throttle: 1, timeout: 120, show: false, page: 'decode/', shot: null, lang: null };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--throttle') options.throttle = Number(argv[++index]);
    else if (argument === '--timeout') options.timeout = Number(argv[++index]);
    else if (argument === '--show') options.show = true;
    else if (argument === '--page') options.page = argv[++index];
    else if (argument === '--shot') options.shot = argv[++index];
    else if (argument === '--lang') options.lang = argv[++index];
    else positional.push(argument);
  }
  return { positional, options };
}

function chromePath() {
  const candidates = [
    process.env.CHROME,
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  ];
  return candidates.find((path) => path && existsSync(path));
}

/** Serves the assembled site on a free local port. `localhost` is a secure
 * context, so the camera is offered without a certificate. */
function serve() {
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://localhost');
    let path = decodeURIComponent(url.pathname);
    if (path.endsWith('/')) path += 'index.html';
    const target = join(site, normalize(path).replace(/^(\.\.[/\\])+/, ''));
    if (!target.startsWith(site)) {
      response.writeHead(403).end();
      return;
    }
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
  return new Promise((done) => {
    server.listen(0, '127.0.0.1', () => done(server));
  });
}

const { positional, options } = parseArguments(process.argv.slice(2));
if (positional.length < 2) {
  console.error('usage: node tools/e2e/receive.mjs <camera.y4m> <original-file> [--throttle N] [--timeout S]');
  process.exit(2);
}

const recording = resolve(positional[0]);
const original = await readFile(resolve(positional[1]));
const wanted = createHash('sha256').update(original).digest('hex');

if (!existsSync(join(site, 'decode', 'index.html'))) {
  console.error('there is no assembled site. Build it first:');
  console.error('  wasm-pack build wasm --release --target web --out-dir pkg');
  console.error('  node tools/build-site.mjs');
  process.exit(2);
}

const executablePath = chromePath();
if (!executablePath) {
  console.error('no Chrome found; set CHROME to its path');
  process.exit(2);
}

const server = await serve();
const { port } = server.address();

const browser = await puppeteer.launch({
  executablePath,
  headless: !options.show,
  args: [
    '--use-fake-ui-for-media-stream',
    '--use-fake-device-for-media-stream',
    `--use-file-for-fake-video-capture=${recording}`,
    '--autoplay-policy=no-user-gesture-required',
    '--no-first-run',
    '--disable-background-timer-throttling',
    '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows',
  ],
});

let verdict = 1;

try {
  const page = await browser.newPage();
  // A phone held upright.
  await page.setViewport({ width: 412, height: 915, deviceScaleFactor: 2, isMobile: true, hasTouch: true });

  page.on('console', (message) => {
    const text = message.text();
    if (message.type() === 'error' || text.startsWith('[photon]')) console.log(`  page: ${text}`);
  });
  page.on('pageerror', (error) => console.log(`  page error: ${error.message}`));

  if (options.throttle > 1) {
    // The page is slowed by the browser. The decoder runs in a worker, which
    // the browser will not slow, so the page is asked to slow that itself.
    const session = await page.createCDPSession();
    await session.send('Emulation.setCPUThrottlingRate', { rate: options.throttle });
  }
  const parameters = new URLSearchParams();
  if (options.throttle > 1) parameters.set('slow', String(options.throttle));
  if (options.lang) parameters.set('lang', options.lang);
  const query = parameters.size > 0 ? `?${parameters}` : '';

  await page.goto(`http://127.0.0.1:${port}/${options.page}${query}`, { waitUntil: 'networkidle0' });
  await page.waitForFunction(() => !document.getElementById('start').disabled, { timeout: 30000 });

  const started = Date.now();
  await page.click('#start');

  let last = '';
  let outcome = null;
  let pictured = false;
  while (Date.now() - started < options.timeout * 1000) {
    const state = await page.evaluate(() => {
      const text = (id) => document.getElementById(id)?.textContent ?? '';
      const status = document.getElementById('status');
      const download = document.getElementById('download');
      return {
        status: status && !status.classList.contains('hidden') ? status.textContent : '',
        bad: status?.classList.contains('bad') ?? false,
        ready: Boolean(
          download?.getAttribute('href') &&
            !document.getElementById('result').classList.contains('hidden'),
        ),
        blocks: text('fact-blocks'),
        perCell: text('fact-percell'),
        frames: text('fact-frames'),
        used: text('fact-used'),
        repeated: text('fact-repeated'),
        missed: text('fact-missed'),
        straddled: text('fact-straddled'),
        header: text('fact-header'),
        cells: text('fact-cells'),
        extra: window.photonStats ? JSON.stringify(window.photonStats) : '',
      };
    });

    const line =
      `${((Date.now() - started) / 1000).toFixed(1).padStart(6)}s  ` +
      `blocks ${state.blocks.padEnd(18)} read ${state.frames.padEnd(5)} new ${state.used.padEnd(4)} ` +
      `again ${state.repeated.padEnd(4)} ` +
      `lost: find ${state.missed} mid-change ${state.straddled} header ${state.header} cells ${state.cells}  ` +
      `${state.perCell} px/cell ${state.extra}`;
    if (line.slice(9) !== last) {
      console.log(line);
      last = line.slice(9);
    }

    // What the person holding the phone is looking at, part-way through.
    if (options.shot && !pictured && Date.now() - started > 3000) {
      pictured = true;
      await page.screenshot({ path: `${options.shot}-during.png` });
    }

    if (state.ready) {
      outcome = { ok: true, seconds: (Date.now() - started) / 1000 };
      if (options.shot) await page.screenshot({ path: `${options.shot}-after.png`, fullPage: true });
      break;
    }
    if (state.bad) {
      outcome = { ok: false, reason: state.status };
      break;
    }
    await new Promise((done) => setTimeout(done, 1000));
  }

  if (!outcome) {
    console.log(`\nFAILED  nothing was offered within ${options.timeout} s`);
  } else if (!outcome.ok) {
    console.log(`\nFAILED  ${outcome.reason}`);
  } else {
    const received = await page.evaluate(async () => {
      const link = document.getElementById('download');
      const bytes = new Uint8Array(await (await fetch(link.href)).arrayBuffer());
      let binary = '';
      for (let index = 0; index < bytes.length; index += 0x8000) {
        binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
      }
      return { name: link.download, base64: btoa(binary) };
    });
    const bytes = Buffer.from(received.base64, 'base64');
    const got = createHash('sha256').update(bytes).digest('hex');

    if (got === wanted) {
      const rate = original.length / outcome.seconds / 1024;
      console.log(
        `\nPASSED  ${received.name}, ${bytes.length} bytes, digest matches, ` +
          `${outcome.seconds.toFixed(1)} s, ${rate.toFixed(1)} KB/s`,
      );
      // This script looks once a second, which is a long time in a transfer
      // of four. The page timed it itself, from the first block it was given.
      const said = await page.evaluate(() => document.getElementById('status')?.textContent ?? '');
      console.log(`        the page says: ${said}`);
      verdict = 0;
    } else {
      console.log(`\nFAILED  the page offered ${bytes.length} bytes that are not the file that was sent`);
    }
  }
} finally {
  await browser.close();
  server.close();
}

process.exit(verdict);
