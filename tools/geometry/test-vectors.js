// Compute the normative test vectors embedded in SPEC.md.
const crypto = require('crypto');

function crc16Ccitt(buf) { // CRC-16/CCITT-FALSE: poly 0x1021, init 0xFFFF, no reflect, xorout 0
  let crc = 0xFFFF;
  for (const b of buf) {
    crc ^= b << 8;
    for (let i = 0; i < 8; i++) crc = (crc & 0x8000) ? ((crc << 1) ^ 0x1021) & 0xFFFF : (crc << 1) & 0xFFFF;
  }
  return crc;
}

let CRC32C_TABLE = null;
function crc32c(buf) { // CRC-32/ISCSI (Castagnoli), reflected 0x82F63B78, init/xorout 0xFFFFFFFF
  if (!CRC32C_TABLE) {
    CRC32C_TABLE = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = (c & 1) ? (0x82F63B78 ^ (c >>> 1)) : (c >>> 1);
      CRC32C_TABLE[n] = c >>> 0;
    }
  }
  let crc = 0xFFFFFFFF;
  for (const b of buf) crc = (CRC32C_TABLE[(crc ^ b) & 0xFF] ^ (crc >>> 8)) >>> 0;
  return (crc ^ 0xFFFFFFFF) >>> 0;
}

const hex = b => Buffer.from(b).toString('hex').toUpperCase().replace(/(..)/g, '$1 ').trim();

// --- sanity checks against published CRC check values ("123456789") ---------
const check = Buffer.from('123456789', 'ascii');
console.log('CRC-16/CCITT-FALSE("123456789") =', crc16Ccitt(check).toString(16).toUpperCase(), '(expected 29B1)');
console.log('CRC-32C("123456789")            =', crc32c(check).toString(16).toUpperCase(), '(expected E3069283)');

// --- Vector 1: frame header (P2-standard) -----------------------------------
const h = Buffer.alloc(20);
Buffer.from('PHTN', 'ascii').copy(h, 0);
h[4] = 0x01;                       // protocol_version
h[5] = 0x02;                       // profile_id = P2-standard
h[6] = 0x01;                       // flags = HAS_MANIFEST
h[7] = 0x07;                       // unit_count
h.writeUInt32LE(0x0BADC0DE, 8);    // session_id
h.writeUInt32LE(0x00000001, 12);   // frame_seq
h.writeUInt16LE(7047, 16);         // payload_len = full P2-standard frame
const hcrc = crc16Ccitt(h.subarray(0, 18));
h.writeUInt16LE(hcrc, 18);
console.log('\n--- frame header ---');
console.log('bytes[0..18) :', hex(h.subarray(0, 18)));
console.log('crc16        : 0x' + hcrc.toString(16).toUpperCase().padStart(4, '0'));
console.log('full 20 bytes:', hex(h));

// --- Vector 2: manifest ------------------------------------------------------
const name = Buffer.from('report.pdf', 'utf8');
const m = Buffer.alloc(61 + name.length);
Buffer.from('PHTM', 'ascii').copy(m, 0);
m[4] = 0x01;                       // manifest_version
m[5] = 0x01;                       // compression = Brotli
m.writeUInt16LE(0x0000, 6);        // flags
m.writeBigUInt64LE(1048576n, 8);   // original_size
Buffer.from('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', 'hex').copy(m, 16); // sha256("abc")
Buffer.from('000008000000048001000108', 'hex').copy(m, 48); // RaptorQ OTI: F=524288 T=1152 Z=1 N=1 Al=8
m[60] = name.length;
name.copy(m, 61);
console.log('\n--- manifest (' + m.length + ' bytes) ---');
console.log(hex(m));
console.log('sha256(manifest) =', crypto.createHash('sha256').update(m).digest('hex').toUpperCase());

// --- Vector 3: the manifest wrapped in a payload unit ------------------------
const u = Buffer.alloc(3 + m.length + 4);
u[0] = 0x01;                       // type = MANIFEST
u.writeUInt16LE(m.length, 1);      // length
m.copy(u, 3);
const ucrc = crc32c(u.subarray(0, 3 + m.length));
u.writeUInt32LE(ucrc, 3 + m.length);
console.log('\n--- payload unit (' + u.length + ' bytes) ---');
console.log('crc32c   : 0x' + ucrc.toString(16).toUpperCase().padStart(8, '0'));
console.log('unit head:', hex(u.subarray(0, 8)));
console.log('unit tail:', hex(u.subarray(u.length - 8)));
console.log('sha256(unit) =', crypto.createHash('sha256').update(u).digest('hex').toUpperCase());

// Frame geometry is derived by ./frame-geometry.js.
