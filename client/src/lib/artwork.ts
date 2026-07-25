/**
 * Lock-screen artwork (#14, spec US 30): a square tile carrying the Call's LED
 * color — the app's one meaningful color, and the whole glanceable signal on a
 * lock screen.
 *
 * It is generated rather than shipped as an asset because it is colored per
 * talkgroup: static files would mean one per palette entry per size. iOS wants
 * *small raster* artwork (96×96 / 128×128 are the sizes it historically got
 * right — docs/research/ios-background-audio.md §4), which `lib/png.ts` encodes
 * from flat color for a couple of kilobytes.
 *
 * Whether iOS actually paints it on the lock screen is part of the real-device
 * manual gate (ADR-0005); no headless browser can answer that.
 */
import { LED_HEX, type LedColor } from './led'
import { encodeIndexedPng, type Rgb } from './png'

/** Square edges to publish, smallest first — the sizes research §4 names. */
export const ARTWORK_SIZES = [96, 128] as const

/** The app's near-black background (`--background`, dark theme). */
const BACKGROUND = '#09090b'
/** Dot and halo as a fraction of the tile, so every size looks the same. */
const DOT = 0.31
const HALO = 0.4
/** How much LED color bleeds into the halo ring around the dot. */
const HALO_MIX = 0.28

/** Encoded artwork per color and size. The tile is a pure function of the two,
 *  and there are eight colors, so every Call after the first reuses a string. */
const cache = new Map<string, string>()

/** The lock-screen tile for a LED color, as a `data:image/png;base64,` URL. */
export function ledArtworkUrl(color: LedColor, size: number): string {
  const key = `${color}@${size}`
  const cached = cache.get(key)
  if (cached !== undefined) return cached

  const url = encodeTile(rgb(LED_HEX[color]), size)
  cache.set(key, url)
  return url
}

/** A lit dot in `led` on the app background, as a complete PNG data URL. */
function encodeTile(led: Rgb, size: number): string {
  const background = rgb(BACKGROUND)
  const palette: Rgb[] = [background, mix(background, led, HALO_MIX), led]

  const pixels = new Uint8Array(size * size)
  const center = (size - 1) / 2
  let at = 0
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const distance = Math.hypot(x - center, y - center) / size
      pixels[at++] = distance <= DOT ? 2 : distance <= HALO ? 1 : 0
    }
  }
  return encodeIndexedPng(size, size, palette, pixels)
}

/** `#rrggbb` as its three channels. */
function rgb(hex: string): Rgb {
  const value = Number.parseInt(hex.slice(1), 16)
  return [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff]
}

/** `from` moved `amount` of the way towards `to`. */
function mix(from: Rgb, to: Rgb, amount: number): Rgb {
  return [0, 1, 2].map((channel) =>
    Math.round(from[channel] + (to[channel] - from[channel]) * amount),
  ) as Rgb
}
