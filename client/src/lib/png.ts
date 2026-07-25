/**
 * A minimal PNG encoder — indexed color, no compression.
 *
 * It exists because the lock-screen artwork (`lib/artwork.ts`) has to be
 * generated per talkgroup at runtime: iOS renders *raster* artwork reliably and
 * SVG unreliably (docs/research/ios-background-audio.md §4), and a canvas is
 * both unavailable under test and overkill for flat color. Indexed color keeps
 * a tile of a few colors to a few kilobytes.
 *
 * Verified against Node's zlib (`png.test.ts` inflates what this deflates and
 * checks every chunk CRC), so a malformed file fails a test rather than
 * rendering as a blank square on someone's lock screen.
 */

/** One palette entry: red, green, blue. */
export type Rgb = [number, number, number]

const SIGNATURE = Uint8Array.of(137, 80, 78, 71, 13, 10, 26, 10)

/**
 * Encode a `width` × `height` image as a `data:image/png;base64,` URL.
 * `indices` is one `palette` index per pixel, row-major.
 */
export function encodeIndexedPng(
  width: number,
  height: number,
  palette: readonly Rgb[],
  indices: Uint8Array,
): string {
  const header = new Uint8Array(13)
  const dimensions = new DataView(header.buffer)
  dimensions.setUint32(0, width)
  dimensions.setUint32(4, height)
  header[8] = 8 // bit depth
  header[9] = 3 // color type 3: palette indices
  // Bytes 10–12 stay zero: deflate, adaptive filtering, no interlacing.

  const png = concat([
    SIGNATURE,
    chunk('IHDR', header),
    chunk('PLTE', Uint8Array.from(palette.flat())),
    chunk('IDAT', deflateStored(scanlines(width, height, indices))),
    chunk('IEND', new Uint8Array(0)),
  ])
  return `data:image/png;base64,${base64(png)}`
}

/** The raw format PNG deflates: each row prefixed by its filter byte. */
function scanlines(
  width: number,
  height: number,
  indices: Uint8Array,
): Uint8Array {
  const raw = new Uint8Array(height * (width + 1))
  for (let row = 0; row < height; row++) {
    raw[row * (width + 1)] = 0 // filter: none
    raw.set(indices.subarray(row * width, (row + 1) * width), row * (width + 1) + 1)
  }
  return raw
}

/** A length-prefixed, CRC-suffixed PNG chunk. */
function chunk(type: string, data: Uint8Array): Uint8Array {
  const out = new Uint8Array(12 + data.length)
  const view = new DataView(out.buffer)
  view.setUint32(0, data.length)
  for (let i = 0; i < 4; i++) out[4 + i] = type.charCodeAt(i)
  out.set(data, 8)
  // The CRC covers the type and the data, but not the length.
  view.setUint32(8 + data.length, crc32(out.subarray(4, 8 + data.length)))
  return out
}

/**
 * `raw` as a zlib stream of *stored* (uncompressed) deflate blocks.
 *
 * A real compressor would shrink a flat tile a lot, but the tile is already a
 * few KB and encoded once per color; shipping a deflate implementation (or
 * awaiting `CompressionStream`, which would make this async) costs more than it
 * saves. Blocks cap at 64 KiB, which is the format's limit rather than an
 * anticipated size — an image that outgrows one block must still encode.
 */
function deflateStored(raw: Uint8Array): Uint8Array {
  const MAX_BLOCK = 0xffff
  const blocks = Math.ceil(raw.length / MAX_BLOCK)
  const out = new Uint8Array(2 + blocks * 5 + raw.length + 4)
  out[0] = 0x78 // deflate, 32 KiB window
  out[1] = 0x01 // no preset dictionary; makes the 0x7801 header divisible by 31

  let at = 2
  for (let start = 0; start < raw.length; start += MAX_BLOCK) {
    const block = raw.subarray(start, start + MAX_BLOCK)
    out[at++] = start + MAX_BLOCK >= raw.length ? 1 : 0 // final-block flag
    out[at++] = block.length & 0xff
    out[at++] = block.length >>> 8
    out[at++] = ~block.length & 0xff
    out[at++] = (~block.length >>> 8) & 0xff
    out.set(block, at)
    at += block.length
  }
  new DataView(out.buffer).setUint32(at, adler32(raw))
  return out
}

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, n) => {
  let c = n
  for (let bit = 0; bit < 8; bit++) {
    c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  }
  return c >>> 0
})

function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff
  for (const byte of bytes) crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8)
  return (crc ^ 0xffffffff) >>> 0
}

/** zlib's checksum over the uncompressed bytes. */
function adler32(bytes: Uint8Array): number {
  let low = 1
  let high = 0
  for (const byte of bytes) {
    low = (low + byte) % 65521
    high = (high + low) % 65521
  }
  return ((high << 16) | low) >>> 0
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((size, part) => size + part.length, 0))
  let at = 0
  for (const part of parts) {
    out.set(part, at)
    at += part.length
  }
  return out
}

function base64(bytes: Uint8Array): string {
  // Chunked so a large image can't blow the argument limit of `fromCharCode`.
  let binary = ''
  for (let at = 0; at < bytes.length; at += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(at, at + 0x8000))
  }
  return btoa(binary)
}
