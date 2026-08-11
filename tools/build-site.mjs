// Assembles the deployable site from the pieces that live beside the code.
//
// The pages are kept where the specification's repository layout puts them —
// `web-emitter/`, `web-decoder/`, `web-shared/` — because that is where someone
// looking for the sending page expects to find it. What gets served is a
// different shape, with short URLs and one copy of the WebAssembly bundle, so
// something has to do the rearranging. Doing it in a script rather than in the
// deployment workflow means it can be run locally too, which is the difference
// between testing the site and hoping.
//
//   node tools/build-site.mjs [outputDir]
//
// Expects `wasm-pack build wasm --release --target web --out-dir pkg` to have
// run first.

import { cp, mkdir, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const out = resolve(process.argv[2] ?? join(root, 'site'));

/** Everything the served site is made of, and where it lands. */
const layout = [
  { from: 'web-shared/index.html', to: 'index.html' },
  { from: 'web-shared/style.css', to: 'shared/style.css' },
  { from: 'web-emitter', to: 'emit' },
  { from: 'web-decoder', to: 'decode' },
  { from: 'wasm/pkg', to: 'photon' },
];

/** Files wasm-pack leaves behind that a web server has no use for. */
const unwanted = ['photon/.gitignore', 'photon/README.md', 'photon/package.json'];

async function build() {
  const missing = layout.map((e) => e.from).filter((p) => !existsSync(join(root, p)));
  if (missing.length > 0) {
    console.error(`missing: ${missing.join(', ')}`);
    if (missing.includes('wasm/pkg')) {
      console.error('build the WebAssembly package first:');
      console.error('  wasm-pack build wasm --release --target web --out-dir pkg');
    }
    process.exit(1);
  }

  await rm(out, { recursive: true, force: true });
  await mkdir(out, { recursive: true });

  for (const entry of layout) {
    await cp(join(root, entry.from), join(out, entry.to), { recursive: true });
  }

  for (const path of unwanted) {
    await rm(join(out, path), { force: true });
  }

  // GitHub Pages runs Jekyll over anything it is given unless told not to, and
  // Jekyll silently drops files and directories whose names begin with an
  // underscore — which is exactly how wasm-bindgen names some of its output.
  await writeFile(join(out, '.nojekyll'), '');

  await report(out);
}

async function report(directory) {
  console.log(`site assembled at ${directory}`);
  let total = 0;

  const walk = async (dir, prefix = '') => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(path, `${prefix}${entry.name}/`);
      } else {
        const { size } = await stat(path);
        total += size;
        if (size > 64 * 1024) {
          console.log(`  ${prefix}${entry.name}  ${(size / 1024).toFixed(0)} KB`);
        }
      }
    }
  };

  await walk(directory);
  console.log(`  total ${(total / 1024).toFixed(0)} KB`);
}

await build();
