/**
 * Decoding side of the PNG tests, checked against Node's zlib rather than
 * against our own encoder: `crc32` verifies every chunk and `inflateSync`
 * verifies the whole deflate stream (adler32 included). A malformed file fails
 * a test here instead of silently rendering as a blank square on a lock screen.
 */
import { crc32, inflateSync } from 'node:zlib'
import { expect } from 'vitest'

const DATA_URL_PREFIX = 'data:image/png;base64,'
const SIGNATURE = [137, 80, 78, 71, 13, 10, 26, 10]

export interface PngChunk {
  type: string
  data: Buffer
}

/** The bytes behind a `data:image/png;base64,` URL. */
export function decodeDataUrl(url: string): Buffer {
  expect(url.startsWith(DATA_URL_PREFIX)).toBe(true)
  return Buffer.from(url.slice(DATA_URL_PREFIX.length), 'base64')
}

/** Walk the chunk stream, asserting the signature and each chunk's declared
 *  length and CRC. A malformed stream fails rather than returning junk. */
export function chunks(png: Buffer): PngChunk[] {
  expect([...png.subarray(0, 8)]).toEqual(SIGNATURE)
  const found: PngChunk[] = []
  let at = 8
  while (at < png.length) {
    const length = png.readUInt32BE(at)
    const type = png.toString('latin1', at + 4, at + 8)
    const data = png.subarray(at + 8, at + 8 + length)
    expect(png.readUInt32BE(at + 8 + length)).toBe(
      crc32(png.subarray(at + 4, at + 8 + length)),
    )
    found.push({ type, data })
    at += 12 + length
  }
  expect(at).toBe(png.length)
  return found
}

export function chunk(png: Buffer, type: string): Buffer {
  const match = chunks(png).find((entry) => entry.type === type)
  expect(match, `${type} chunk`).toBeDefined()
  return match!.data
}

/** The inflated palette indices, one per pixel, row-major. */
export function pixels(png: Buffer, width: number, height: number): Buffer {
  const raw = inflateSync(chunk(png, 'IDAT'))
  // Each scanline is a filter byte (0 = none) plus one index per pixel.
  expect(raw.length).toBe(height * (width + 1))
  const out = Buffer.alloc(width * height)
  for (let row = 0; row < height; row++) {
    const start = row * (width + 1)
    expect(raw[start]).toBe(0)
    raw.copy(out, row * width, start + 1, start + 1 + width)
  }
  return out
}
