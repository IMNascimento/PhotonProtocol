// Search for a blur-tolerant set of 4x4 balanced shape masks.
// Model: each mask is rendered on a 4x supersampled grid (16x16), blurred with a
// Gaussian of sigma = 0.5 cell, sampled with a sub-cell registration error, and
// reduced to a 4x4 feature vector (what a decoder actually measures).
// Objective: maximise the minimum feature distance over all mask pairs AND all
// registration-error combinations (worst case).

const SS = 4;              // supersample factor per shape sub-cell
const N = 4 * SS;          // 16x16 supersampled image
const SIGMA = 0.5 * SS;    // 0.5 sub-cell of blur
const SHIFTS = [[0, 0], [0.35, 0], [-0.35, 0], [0, 0.35], [0, -0.35]]; // in sub-cells

function popcount(x) { let c = 0; while (x) { x &= x - 1; c++; } return c; }

// perimeter = number of 4-neighbour adjacencies with differing value (chunkiness)
function perimeter(m) {
  const at = (r, c) => (r < 0 || c < 0 || r > 3 || c > 3) ? 0 : (m >> (r * 4 + c)) & 1;
  let p = 0;
  for (let r = 0; r < 4; r++) for (let c = 0; c < 4; c++) {
    if (at(r, c) !== at(r, c + 1) && c < 3) p++;
    if (at(r, c) !== at(r + 1, c) && r < 3) p++;
  }
  return p;
}

function gaussKernel(sigma) {
  const rad = Math.ceil(3 * sigma);
  const k = [];
  let sum = 0;
  for (let i = -rad; i <= rad; i++) { const v = Math.exp(-(i * i) / (2 * sigma * sigma)); k.push(v); sum += v; }
  return { rad, k: k.map(v => v / sum) };
}
const G = gaussKernel(SIGMA);

function render(mask) {
  const img = new Float64Array(N * N);
  for (let r = 0; r < 4; r++) for (let c = 0; c < 4; c++) {
    const v = (mask >> (r * 4 + c)) & 1;
    for (let y = 0; y < SS; y++) for (let x = 0; x < SS; x++) img[(r * SS + y) * N + c * SS + x] = v;
  }
  return img;
}

function blur(img) {
  const tmp = new Float64Array(N * N);
  const out = new Float64Array(N * N);
  const clamp = i => Math.min(N - 1, Math.max(0, i));
  for (let y = 0; y < N; y++) for (let x = 0; x < N; x++) {
    let s = 0;
    for (let i = -G.rad; i <= G.rad; i++) s += G.k[i + G.rad] * img[y * N + clamp(x + i)];
    tmp[y * N + x] = s;
  }
  for (let y = 0; y < N; y++) for (let x = 0; x < N; x++) {
    let s = 0;
    for (let i = -G.rad; i <= G.rad; i++) s += G.k[i + G.rad] * tmp[clamp(y + i) * N + x];
    out[y * N + x] = s;
  }
  return out;
}

function sampleBilinear(img, y, x) {
  const cl = v => Math.min(N - 1, Math.max(0, v));
  const y0 = Math.floor(y), x0 = Math.floor(x);
  const fy = y - y0, fx = x - x0;
  const g = (yy, xx) => img[cl(yy) * N + cl(xx)];
  return g(y0, x0) * (1 - fy) * (1 - fx) + g(y0, x0 + 1) * (1 - fy) * fx
       + g(y0 + 1, x0) * fy * (1 - fx) + g(y0 + 1, x0 + 1) * fy * fx;
}

// feature: 4x4 average of the blurred image, sampled with a registration offset
function feature(blurred, dy, dx) {
  const f = new Float64Array(16);
  for (let r = 0; r < 4; r++) for (let c = 0; c < 4; c++) {
    let s = 0;
    for (let y = 0; y < SS; y++) for (let x = 0; x < SS; x++) {
      s += sampleBilinear(blurred, r * SS + y + 0.5 + dy, c * SS + x + 0.5 + dx);
    }
    f[r * 4 + c] = s / (SS * SS);
  }
  return f;
}

function dist(fa, fb) {
  let s = 0;
  for (let i = 0; i < 16; i++) { const d = fa[i] - fb[i]; s += d * d; }
  return Math.sqrt(s);
}

// worst-case distance between two masks over all registration-offset pairs
function worstDist(A, B) {
  let m = Infinity;
  for (const fa of A) for (const fb of B) { const d = dist(fa, fb); if (d < m) m = d; }
  return m;
}

// ---- candidate generation -------------------------------------------------
const PERIM_MAX = Number(process.argv[2] || 10);
const cands = [];
for (let m = 0; m < 65536; m++) {
  if (popcount(m) !== 8) continue;           // balanced ink -> uniform chroma energy
  if (perimeter(m) > PERIM_MAX) continue;    // chunky shapes survive blur
  cands.push(m);
}
console.log(`candidates (balanced, perimeter<=${PERIM_MAX}): ${cands.length}`);

const feats = cands.map(m => {
  const b = blur(render(m));
  return SHIFTS.map(([dy, dx]) => feature(b, dy, dx));
});

// pairwise worst-case distance matrix
const n = cands.length;
const D = [];
for (let i = 0; i < n; i++) {
  D.push(new Float64Array(n));
}
for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) {
  const d = worstDist(feats[i], feats[j]);
  D[i][j] = d; D[j][i] = d;
}

// ---- max-min set selection: greedy from every seed, then local improvement --
function minPair(set) {
  let m = Infinity;
  for (let i = 0; i < set.length; i++) for (let j = i + 1; j < set.length; j++) {
    if (D[set[i]][set[j]] < m) m = D[set[i]][set[j]];
  }
  return m;
}

function select(k) {
  let best = null, bestScore = -1;
  for (let seed = 0; seed < n; seed++) {
    const set = [seed];
    while (set.length < k) {
      let bi = -1, bv = -1;
      for (let c = 0; c < n; c++) {
        if (set.includes(c)) continue;
        let v = Infinity;
        for (const s of set) v = Math.min(v, D[s][c]);
        if (v > bv) { bv = v; bi = c; }
      }
      if (bi < 0) break;
      set.push(bi);
    }
    // local improvement: try swapping each member for any candidate
    let improved = true;
    while (improved) {
      improved = false;
      for (let idx = 0; idx < set.length; idx++) {
        const cur = minPair(set);
        for (let c = 0; c < n; c++) {
          if (set.includes(c)) continue;
          const old = set[idx]; set[idx] = c;
          if (minPair(set) > cur) { improved = true; break; }
          set[idx] = old;
        }
        if (improved) break;
      }
    }
    const sc = minPair(set);
    if (sc > bestScore) { bestScore = sc; best = set.slice(); }
  }
  return { set: best.map(i => cands[i]), score: bestScore, idx: best };
}

function show(mask) {
  const rows = [];
  for (let r = 0; r < 4; r++) {
    let s = '';
    for (let c = 0; c < 4; c++) s += ((mask >> (r * 4 + c)) & 1) ? '#' : '.';
    rows.push(s);
  }
  return rows;
}

for (const k of [4, 8]) {
  const res = select(k);
  console.log(`\n=== ${k} shapes, worst-case min feature distance = ${res.score.toFixed(4)} ===`);
  // stable, deterministic order: ascending mask value
  const ordered = res.set.slice().sort((a, b) => a - b);
  ordered.forEach((m, i) => {
    console.log(`shape ${i}: 0x${m.toString(16).padStart(4, '0').toUpperCase()}  perim=${perimeter(m)}`);
    show(m).forEach(r => console.log('        ' + r));
  });
  // report the raw min distance without registration error, for reference
  let mn = Infinity;
  for (let i = 0; i < res.idx.length; i++) for (let j = i + 1; j < res.idx.length; j++) {
    mn = Math.min(mn, dist(feats[res.idx[i]][0], feats[res.idx[j]][0]));
  }
  console.log(`aligned (no registration error) min distance = ${mn.toFixed(4)}`);
}
