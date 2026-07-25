import { describe, expect, it } from 'vitest'

import { chunk, decodeDataUrl, pixels } from '@/test/png'

import { ARTWORK_SIZES, ledArtworkUrl } from './artwork'
import { LED_HEX } from './led'

/** The tile decoded at `size`, as palette indices plus its palette. */
function tile(color: keyof typeof LED_HEX, size: number) {
  const png = decodeDataUrl(ledArtworkUrl(color, size))
  const px = pixels(png, size, size)
  const palette = chunk(png, 'PLTE')
  return {
    at: (x: number, y: number) => px[y * size + x],
    color: (index: number) =>
      `#${palette.subarray(index * 3, index * 3 + 3).toString('hex')}`,
  }
}

describe('lock-screen artwork', () => {
  /** Research §4: iOS renders small artwork reliably, and wants more than one
   *  size to choose from. */
  it('publishes small sizes only', () => {
    expect(ARTWORK_SIZES.length).toBeGreaterThan(1)
    for (const size of ARTWORK_SIZES) {
      expect(size).toBeLessThanOrEqual(128)
      const png = decodeDataUrl(ledArtworkUrl('red', size))
      expect(chunk(png, 'IHDR').readUInt32BE(0)).toBe(size)
    }
  })

  it.each(ARTWORK_SIZES)(
    'paints the talkgroup LED color on the app background at %ipx',
    (size) => {
      const green = tile('green', size)

      expect(green.color(green.at(size / 2, size / 2))).toBe(LED_HEX.green)
      // The corner is the app's near-black, so the LED reads as a lit dot.
      expect(green.color(green.at(0, 0))).toBe('#09090b')
    },
  )

  it('rings the dot with a dimmed halo, so it reads as lit rather than flat', () => {
    const size = ARTWORK_SIZES[0]
    const cyan = tile('cyan', size)
    const middle = size / 2

    // Walking out from the center: dot, then halo, then background.
    const walk = Array.from({ length: middle }, (_, x) => cyan.at(middle + x, middle))
    expect(new Set(walk)).toEqual(new Set([0, 1, 2]))
    expect(walk[0]).toBe(2)
    expect(walk.at(-1)).toBe(0)
    // Monotonic: the dot never reappears outside the halo.
    expect([...walk].sort((a, b) => b - a)).toEqual(walk)
  })

  it('gives every LED color its own artwork, and reuses each one', () => {
    const urls = Object.keys(LED_HEX).map((color) =>
      ledArtworkUrl(color as keyof typeof LED_HEX, ARTWORK_SIZES[0]),
    )

    expect(new Set(urls).size).toBe(urls.length)
    // Memoized per color *and* size: encoded once, not once per Call.
    expect(ledArtworkUrl('blue', 96)).toBe(ledArtworkUrl('blue', 96))
    expect(ledArtworkUrl('blue', 96)).not.toBe(ledArtworkUrl('blue', 128))
  })
})
