import { describe, expect, it } from 'vitest'

import { chunk, chunks, decodeDataUrl, pixels } from '@/test/png'

import { encodeIndexedPng, type Rgb } from './png'

const RED: Rgb = [255, 0, 0]
const BLACK: Rgb = [0, 0, 0]

describe('indexed PNG encoder', () => {
  it('declares the size and color type it was given', () => {
    const png = decodeDataUrl(
      encodeIndexedPng(3, 2, [BLACK, RED], Uint8Array.of(0, 1, 0, 1, 0, 1)),
    )

    const header = chunk(png, 'IHDR')
    expect(header.readUInt32BE(0)).toBe(3)
    expect(header.readUInt32BE(4)).toBe(2)
    expect(header[8]).toBe(8) // bit depth
    expect(header[9]).toBe(3) // color type 3 — indexed, so the file stays tiny
    expect(chunks(png).at(-1)?.type).toBe('IEND')
  })

  it('round-trips the palette and every pixel', () => {
    const indices = Uint8Array.of(0, 1, 1, 0)

    const png = decodeDataUrl(encodeIndexedPng(2, 2, [BLACK, RED], indices))

    expect([...chunk(png, 'PLTE')]).toEqual([0, 0, 0, 255, 0, 0])
    expect([...pixels(png, 2, 2)]).toEqual([...indices])
  })

  /** Stored deflate blocks cap at 64 KiB, so a larger image has to span
   *  several — and only the last may carry the final-block flag. */
  it('spans several deflate blocks for an image past 64 KiB', () => {
    const width = 400
    const height = 200 // 400 × 200 plus a filter byte per row = 80,200 bytes
    const indices = Uint8Array.from({ length: width * height }, (_, at) =>
      at % 2 === 0 ? 0 : 1,
    )

    const png = decodeDataUrl(encodeIndexedPng(width, height, [BLACK, RED], indices))

    expect([...pixels(png, width, height)]).toEqual([...indices])
  })
})
