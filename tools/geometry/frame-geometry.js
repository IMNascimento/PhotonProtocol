// Final frame geometry per SPEC.md section 4/5.
const profiles = [
  { id: 0x01, name: 'P1-conservative', g: 96,  shapes: 4, colors: 4, k: 175 },
  { id: 0x02, name: 'P2-standard',     g: 128, shapes: 8, colors: 4, k: 199 },
  { id: 0x03, name: 'P3-dense',        g: 160, shapes: 8, colors: 8, k: 223 },
];

const rows = [];
for (const p of profiles) {
  const b = Math.log2(p.shapes) + Math.log2(p.colors);
  const W = p.g - 16;                       // usable cells per ring edge / header row
  const Hr = Math.ceil(320 / W);            // header band height, >= 20 bytes of data
  const nh = Math.floor(Hr * W / 8);        // header codeword length in bytes
  const hSpare = Hr * W - nh * 8;           // leftover cells in each header band
  const reserved = 314 + (8 + 2 * Hr) * W;  // finders + rings + 2 header bands + tag + align
  const data = p.g * p.g - reserved;
  const raw = Math.floor(data * b / 8);
  const par = 255 - p.k;
  const full = Math.floor(raw / 255);
  const rem = raw - full * 255;
  const useShort = rem >= par + 1;
  const shortN = useShort ? rem : 0;
  const shortK = useShort ? rem - par : 0;
  const cap = full * p.k + shortK;
  const dropped = useShort ? 0 : rem;
  rows.push({ ...p, b, W, Hr, nh, hSpare, reserved, data, raw, full, shortN, shortK, cap, dropped,
              rsRate: (par / 255 * 100).toFixed(1), overhead: ((1 - cap / raw) * 100).toFixed(1),
              reservedPct: (reserved / (p.g * p.g) * 100).toFixed(1) });
}

for (const r of rows) {
  console.log(`\n${r.name}  (id 0x${r.id.toString(16).padStart(2, '0')})`);
  console.log(`  grid                 ${r.g} x ${r.g} = ${r.g * r.g} cells`);
  console.log(`  bits/cell            ${r.b}  (${r.shapes} shapes x ${r.colors} colours)`);
  console.log(`  header band height   Hr = ${r.Hr} rows, codeword RS(${r.nh},20), spare cells ${r.hSpare}`);
  console.log(`  reserved cells       ${r.reserved}  (${r.reservedPct}%)`);
  console.log(`  data cells           ${r.data}`);
  console.log(`  raw payload bytes    ${r.raw}`);
  console.log(`  RS(255,${r.k})        ${r.full} full codewords + shortened RS(${r.shortN},${r.shortK}), ${r.rsRate}% parity`);
  console.log(`  payload capacity     ${r.cap} bytes  (link overhead ${r.overhead}%)`);
  if (r.dropped) console.log(`  !! ${r.dropped} trailing raw bytes unusable`);
}

console.log('\n\nmarkdown table:\n');
console.log('| profile | id | grid | shapes | colours | bits/cell | header | RS | data cells | payload capacity |');
console.log('| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |');
for (const r of rows) {
  console.log(`| \`${r.name}\` | \`0x${r.id.toString(16).padStart(2, '0')}\` | ${r.g}x${r.g} | ${r.shapes} | ${r.colors} | ${r.b} | RS(${r.nh},20) x2 | RS(255,${r.k}) | ${r.data} | ${r.cap} B |`);
}
