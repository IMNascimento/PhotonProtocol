// Films a transfer under a table of conditions and says how each one went.
//
//   node tools/e2e/matrix.mjs <file> [--seconds S] [--only name]
//                                     [--save results.json] [--against results.json]
//
// `--save` keeps the results, and `--against` compares this run with results
// kept earlier, row by row. A change that is meant to make transfers faster
// has to be shown to, against the figures from before it.
//
// One capture that works says the decoder can work. A table says where it
// stops: which camera, how far away, how fast the codes change. It is also
// what a change has to be run against before anyone is asked to pick up a
// phone, because a change that helps one row routinely costs another.
//
// Uses `photon film` and `photon decode`, so it needs the command line built:
//
//   cargo build --release -p photon-cli

import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..', '..');
const photon = join(root, 'target', 'release', 'photon');

const positional = [];
const options = { seconds: 3, only: null, save: null, against: null };
const argv = process.argv.slice(2);
for (let index = 0; index < argv.length; index += 1) {
  if (argv[index] === '--seconds') options.seconds = Number(argv[++index]);
  else if (argv[index] === '--only') options.only = argv[++index];
  else if (argv[index] === '--save') options.save = argv[++index];
  else if (argv[index] === '--against') options.against = argv[++index];
  else positional.push(argv[index]);
}

if (positional.length < 1) {
  console.error(
    'usage: node tools/e2e/matrix.mjs <file> [--seconds S] [--only name] [--save json] [--against json]',
  );
  process.exit(2);
}
if (!existsSync(photon)) {
  console.error('build the command line first: cargo build --release -p photon-cli');
  process.exit(2);
}

const file = resolve(positional[0]);

// The camera runs a little off thirty frames a second on purpose. At exactly
// thirty it would keep step with a sixty-hertz screen, every picture would
// catch the code at the same point in its life, and a run would test one
// phase rather than all of them.
const base = { '--fps': '29.5', '--refresh': '59.95' };

/** The base with a scenario's own arguments laid over it. */
function argumentsFor(extra) {
  const merged = { ...base };
  for (let index = 0; index < extra.length; index += 2) merged[extra[index]] = extra[index + 1];
  return Object.entries(merged).flat();
}

/** name, arguments, and the fewest codes a second that counts as working. */
const scenarios = [
  ['ideal', ['--preset', 'ideal', '--hold', '6'], 9],
  ['good', ['--preset', 'good', '--hold', '6'], 9],
  ['typical', ['--preset', 'typical', '--hold', '6'], 9],
  ['poor', ['--preset', 'poor', '--hold', '6'], 7],
  ['typical, 15 codes/s', ['--preset', 'typical', '--hold', '4'], 12],
  ['typical, 20 codes/s', ['--preset', 'typical', '--hold', '3'], 12],
  ['typical, 144 Hz screen', ['--preset', 'typical', '--hold', '14', '--refresh', '143.9'], 9],
  ['typical, further away', ['--preset', 'typical', '--hold', '6', '--fill', '0.6'], 9],
  ['typical, shutter rolling down', ['--preset', 'typical', '--hold', '6', '--sweep', 'down'], 9],
  ['typical, landscape', ['--preset', 'typical', '--hold', '6', '--camera', '1920x1080', '--fill', '0.85'], 9],
  ['typical, 720p', ['--preset', 'typical', '--hold', '6', '--camera', '720x1280', '--fill', '0.9'], 8],
  ['typical, 4K', ['--preset', 'typical', '--hold', '6', '--camera', '2160x3840'], 9],
  ['typical, 1440p screen', ['--preset', 'typical', '--hold', '6', '--screen', '2560x1440'], 9],
  ['typical, shaky', ['--preset', 'typical', '--hold', '6', '--shake', '0.6'], 8],
  ['typical, overexposed', ['--preset', 'typical', '--hold', '6', '--gain', '1.6'], 8],
  ['typical, bent lens', ['--preset', 'typical', '--hold', '6', '--distortion', '0.04', '--fill', '0.95'], 8],
  ['P4-balanced, good', ['--profile', 'p4', '--preset', 'good', '--hold', '6'], 9],
  ['P4-balanced, typical', ['--profile', 'p4', '--preset', 'typical', '--hold', '6'], 9],
  ['P4-balanced, typical, 15 codes/s', ['--profile', 'p4', '--preset', 'typical', '--hold', '4', '--fill', '0.92'], 12],
  ['P4-balanced, poor, close up', ['--profile', 'p4', '--preset', 'poor', '--hold', '6', '--fill', '0.9'], 8],
  ['P2-standard, typical, 4K', ['--profile', 'p2', '--preset', 'typical', '--hold', '6', '--camera', '2160x3840'], 9],
  ['P3-dense, typical, 4K', ['--profile', 'p3', '--preset', 'typical', '--hold', '6', '--camera', '2160x3840', '--fill', '0.9'], 9],
];

const work = mkdtempSync(join(tmpdir(), 'photon-matrix-'));
let failures = 0;
const results = [];
const before = options.against
  ? new Map(JSON.parse(readFileSync(resolve(options.against), 'utf8')).rows.map((row) => [row.name, row]))
  : null;

console.log(
  `${'CONDITIONS'.padEnd(36)}${'PX/CELL'.padStart(8)}${'CODES/S'.padStart(9)}` +
    `${'OF'.padStart(6)}${'KB/S'.padStart(8)}${'MEDIAN WRONG'.padStart(14)}${'LOST'.padStart(7)}  VERDICT` +
    `${before ? '  WAS KB/S' : ''}`,
);

try {
  for (const [name, extra, floor] of scenarios) {
    if (options.only && !name.includes(options.only)) continue;

    const frames = join(work, 'frames');
    rmSync(frames, { recursive: true, force: true });

    const filmed = execFileSync(
      photon,
      ['film', file, '--out', frames, '--seconds', String(options.seconds), ...argumentsFor(extra)],
      { encoding: 'utf8' },
    );
    const shown = Number(/Codes\s+([\d.]+) per second/.exec(filmed)?.[1] ?? 0);

    let decoded = '';
    try {
      decoded = execFileSync(
        photon,
        ['decode', frames, '--out', join(work, 'out'), '--truth', file],
        { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] },
      );
    } catch (error) {
      // A transfer too short to finish is still a measurement.
      decoded = error.stdout ?? '';
    }

    const count = (label) => Number(new RegExp(`^${label}\\s+(\\d+)`, 'm').exec(decoded)?.[1] ?? 0);
    const codes = count('decoded');
    const lost = count('not located') + count('header unreadable');
    const perCell = /Pixels per cell\s+([\d.]+)/.exec(decoded)?.[1] ?? '—';
    const median = /median ([\d.]+)%/.exec(decoded)?.[1] ?? '—';

    const capacity = Number(/Carries\s+(\d+) bytes/.exec(filmed)?.[1] ?? 0);
    const rate = codes / options.seconds;
    const ok = rate >= floor;
    if (!ok) failures += 1;

    const throughput = (rate * capacity) / 1024;
    results.push({
      name,
      arguments: argumentsFor(extra),
      pixelsPerCell: Number(perCell) || null,
      codesShown: shown,
      codesRead: Number(rate.toFixed(2)),
      bytesPerCode: capacity,
      kilobytesPerSecond: Number(throughput.toFixed(1)),
      medianWrongPercent: Number(median) || 0,
      picturesLost: lost,
      ok,
    });
    const was = before?.get(name);
    const change = was
      ? `  ${was.kilobytesPerSecond.toFixed(1).padStart(8)} (${throughput >= was.kilobytesPerSecond ? '+' : ''}${(
          ((throughput - was.kilobytesPerSecond) / Math.max(was.kilobytesPerSecond, 0.1)) *
          100
        ).toFixed(0)}%)`
      : before
        ? '       new'
        : '';

    console.log(
      `${name.padEnd(36)}${perCell.padStart(8)}${rate.toFixed(1).padStart(9)}` +
        `${shown.toFixed(0).padStart(6)}${throughput.toFixed(1).padStart(8)}` +
        `${`${median}%`.padStart(14)}${String(lost).padStart(7)}` +
        `  ${ok ? 'ok    ' : 'FAILED'}${change}`,
    );
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (options.save) {
  const kept = {
    seconds: options.seconds,
    file: basename(file),
    bytes: readFileSync(file).length,
    rows: results,
  };
  writeFileSync(resolve(options.save), `${JSON.stringify(kept, null, 2)}\n`);
  console.log(`\nkept in ${options.save}`);
}

console.log();
console.log('CODES/S is distinct codes read, of the OF shown, by a decoder that looks at');
console.log('every picture. KB/S is what those codes carried. LOST is pictures in which');
console.log('the code was not found or its header would not read.');
process.exit(failures === 0 ? 0 : 1);
