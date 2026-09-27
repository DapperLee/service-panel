// 零依赖生成合法的 PNG（RGBA）与 ICO（内嵌 PNG）图标。
// 用法: node scripts/gen_icons.mjs
import zlib from "node:zlib";
import fs from "node:fs";
import path from "node:path";

const OUT = path.resolve("src-tauri/icons");
fs.mkdirSync(OUT, { recursive: true });

// ---- CRC32 ----
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])));
  return Buffer.concat([len, typeBuf, data, crc]);
}

// 画一个带渐变圆角观感的纯色块（品牌蓝底 + 白色方块）
function makePng(size) {
  const rgba = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const i = (y * size + x) * 4;
      // 背景：深蓝到靛蓝渐变
      let r = 37 + Math.floor((40 * x) / size);
      let g = 99 + Math.floor((30 * y) / size);
      let b = 235;
      // 中间画一个白色"服务方块"标识
      const cx = size / 2,
        cy = size / 2,
        s = size * 0.22;
      if (Math.abs(x - cx) < s && Math.abs(y - cy) < s) {
        r = g = b = 255;
      }
      rgba[i] = r;
      rgba[i + 1] = g;
      rgba[i + 2] = b;
      rgba[i + 3] = 255;
    }
  }
  // 加 filter 字节 0
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type RGBA
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function makeIco(pngList) {
  // pngList: [{size, data}]，全部用 PNG 编码（Vista+ 支持）
  const count = pngList.length;
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2); // type = icon
  header.writeUInt16LE(count, 4);
  const entries = [];
  let offset = 6 + 16 * count;
  for (const { size, data } of pngList) {
    const e = Buffer.alloc(16);
    e[0] = size >= 256 ? 0 : size;
    e[1] = size >= 256 ? 0 : size;
    e[2] = 0;
    e[3] = 0;
    e.writeUInt16LE(1, 4); // planes
    e.writeUInt16LE(32, 6); // bitCount
    e.writeUInt32LE(data.length, 8);
    e.writeUInt32LE(offset, 12);
    entries.push(e);
    offset += data.length;
  }
  return Buffer.concat([header, ...entries, ...pngList.map((p) => p.data)]);
}

const png32 = makePng(32);
const png128 = makePng(128);
const png256 = makePng(256);
const png512 = makePng(512);

fs.writeFileSync(path.join(OUT, "32x32.png"), png32);
fs.writeFileSync(path.join(OUT, "128x128.png"), png128);
fs.writeFileSync(path.join(OUT, "128x128@2x.png"), png256);
fs.writeFileSync(path.join(OUT, "icon.png"), png512);
fs.writeFileSync(path.join(OUT, "Square30x30Logo.png"), png32);
fs.writeFileSync(
  path.join(OUT, "icon.ico"),
  makeIco([
    { size: 32, data: png32 },
    { size: 256, data: png256 },
  ])
);

console.log("icons generated in", OUT);
