// Serves the assembled site over HTTPS, on this machine and to the network.
//
//   node tools/serve.mjs [port]
//
// HTTPS rather than plain HTTP, and that is not a nicety. A browser only grants
// camera access in a secure context, and `http://192.168.x.x` is not one — so a
// phone loading the receiving page over a plain-HTTP LAN address is refused the
// camera with an error that looks like a bug in the page. `localhost` is exempt
// from that rule and the phone is not localhost, which is exactly the case this
// script exists for.
//
// The certificate is self-signed and generated on first run, so the phone will
// warn once and let you continue. That is the price of not routing a local test
// through somebody else's server.

import { createServer } from 'node:https';
import { createServer as createHttpServer } from 'node:http';
import { readFile, mkdir, writeFile, access } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { execFile } from 'node:child_process';
import { networkInterfaces } from 'node:os';
import { extname, join, normalize, resolve } from 'node:path';
import { promisify } from 'node:util';

const run = promisify(execFile);
const root = resolve(import.meta.dirname, '..');
const site = join(root, 'site');
const certDir = join(root, '.certs');
const keyPath = join(certDir, 'local-key.pem');
const certPath = join(certDir, 'local-cert.pem');
const port = Number(process.argv[2] ?? 8443);

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json; charset=utf-8',
  '.png': 'image/png',
  '.svg': 'image/svg+xml',
};

/** Every address this machine can be reached on. */
function addresses() {
  return Object.values(networkInterfaces())
    .flat()
    .filter((entry) => entry && entry.family === 'IPv4' && !entry.internal)
    .map((entry) => entry.address);
}

/** Makes a self-signed certificate, once. */
async function ensureCertificate() {
  if (existsSync(keyPath) && existsSync(certPath)) return true;

  await mkdir(certDir, { recursive: true });

  // Name every address the phone might use, so the certificate at least matches
  // the host even though nothing will have signed it.
  const names = ['localhost', ...addresses()]
    .map((name, index) => `${/^[\d.]+$/.test(name) ? 'IP' : 'DNS'}.${index + 1} = ${name}`)
    .join('\n');

  const config = `
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = photon-local
[ext]
subjectAltName = @names
[names]
${names}
`;

  const configPath = join(certDir, 'openssl.cnf');
  await writeFile(configPath, config);

  try {
    await run('openssl', [
      'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '825',
      '-keyout', keyPath, '-out', certPath, '-config', configPath,
    ]);
    return true;
  } catch {
    console.warn('openssl is not available, so this will serve plain HTTP.');
    console.warn('That is fine on this machine but a phone will be refused the camera.');
    return false;
  }
}

async function handler(request, response) {
  const url = new URL(request.url, 'https://localhost');
  let path = decodeURIComponent(url.pathname);
  if (path.endsWith('/')) path += 'index.html';

  // Normalise before joining, so `..` cannot walk out of the served directory.
  const target = join(site, normalize(path).replace(/^(\.\.[/\\])+/, ''));
  if (!target.startsWith(site)) {
    response.writeHead(403).end('no');
    return;
  }

  try {
    await access(target);
    const body = await readFile(target);
    response.writeHead(200, {
      'content-type': TYPES[extname(target)] ?? 'application/octet-stream',
      // Never cache during development. Serving a stale WebAssembly bundle
      // after a rebuild is a whole debugging session wasted on a fixed bug.
      'cache-control': 'no-store',
    });
    response.end(body);
  } catch {
    response.writeHead(404, { 'content-type': 'text/plain' }).end('not found');
  }
}

if (!existsSync(site)) {
  console.error('there is no assembled site yet. Build it first:');
  console.error('  wasm-pack build wasm --release --target web --out-dir pkg');
  console.error('  node tools/build-site.mjs');
  process.exit(1);
}

const secure = await ensureCertificate();
const server = secure
  ? createServer({ key: await readFile(keyPath), cert: await readFile(certPath) }, handler)
  : createHttpServer(handler);

server.listen(port, '0.0.0.0', () => {
  const scheme = secure ? 'https' : 'http';
  console.log(`serving ${site}`);
  console.log(`  on this machine   ${scheme}://localhost:${port}/`);
  for (const address of addresses()) {
    console.log(`  on the network    ${scheme}://${address}:${port}/`);
  }
  console.log();
  console.log('Open the sender on the screen you are filming and the receiver on the');
  console.log('phone. The phone will warn about the certificate once; continue past it,');
  console.log('or the camera will not be offered.');
});
