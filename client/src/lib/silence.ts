/**
 * The keep-alive loop: one second of inaudible-but-real audio (#15, spec US 31).
 *
 * # What this is for
 *
 * iOS suspends a backgrounded page shortly after its audio stops, and a
 * suspended page cannot advance its own queue — so a lull between Calls ends
 * the listening session, and the lock-screen buttons go dead with it. The only
 * lever the platform gives us is to never stop playing. This is what plays.
 *
 * # Why it must be *audio* and not silence
 *
 * WebKit decides a page is producing audio in `computeCanProduceAudio()`
 * (`Source/WebCore/html/HTMLMediaElement.cpp`), which inspects exactly four
 * things — playing, has an audio track, not `muted`, `volume != 0` — and
 * **never looks at a sample value**
 * (docs/research/ios-gap-bridging-mechanism.md §2, §8). So digital silence
 * would work at that layer, and attenuating with `volume` or `muted` would
 * provably *not*: muting has been the documented way to tell WebKit "I am not
 * audible" since WebKit bug 140524 (2015).
 *
 * What happens *below* WebKit — `mediaserverd`, the audio session — is
 * undocumented, and no source either confirms or denies that iOS notices an
 * all-zero stream. A ±1-LSB square at 16-bit is ≈ −90 dBFS: inaudible on any
 * hardware, identical in cost, and it removes that entire question from the
 * failure surface. The attenuation is *encoded*, never applied by the player.
 *
 * # Why it is generated rather than shipped
 *
 * Same reason as the Media Session artwork (#14): a few dozen lines beat a
 * binary asset that has to be fetched — and this one has to be available in
 * exactly the situation where the network may not be.
 */

/** Sample rate. Speech-band and small: the loop is 16 kB of samples, and it is
 *  never listened to. */
export const KEEP_ALIVE_HZ = 8_000

/** How long the loop is, in samples. One second — long enough that `loop`
 *  restarts it rarely, short enough to stay tiny. */
const SAMPLES = KEEP_ALIVE_HZ

/** Samples per half-cycle of the ±1 square: a 100 Hz tone, far below anything
 *  a −90 dBFS signal could be heard at, and far above DC. */
const HALF_PERIOD = KEEP_ALIVE_HZ / 200

/** Built once. The element's `src` has to be a stable string, or every render
 *  would hand it a "new" resource and restart playback. */
let loop: string | undefined

/** A `data:` URL for the keep-alive loop. */
export function keepAliveLoopUrl(): string {
  return (loop ??= encodeWav())
}

function encodeWav(): string {
  const bytes = new Uint8Array(44 + SAMPLES * 2)
  const view = new DataView(bytes.buffer)

  // RIFF/WAVE, mono 16-bit PCM — the one container every browser decodes
  // without codec negotiation.
  ascii(view, 0, 'RIFF')
  view.setUint32(4, 36 + SAMPLES * 2, true)
  ascii(view, 8, 'WAVE')
  ascii(view, 12, 'fmt ')
  view.setUint32(16, 16, true) // fmt chunk length
  view.setUint16(20, 1, true) // uncompressed PCM
  view.setUint16(22, 1, true) // channels
  view.setUint32(24, KEEP_ALIVE_HZ, true)
  view.setUint32(28, KEEP_ALIVE_HZ * 2, true) // byte rate
  view.setUint16(32, 2, true) // block align
  view.setUint16(34, 16, true) // bits per sample
  ascii(view, 36, 'data')
  view.setUint32(40, SAMPLES * 2, true)

  for (let sample = 0; sample < SAMPLES; sample += 1) {
    const high = Math.floor(sample / HALF_PERIOD) % 2 === 0
    view.setInt16(44 + sample * 2, high ? 1 : -1, true)
  }

  let binary = ''
  for (const byte of bytes) binary += String.fromCharCode(byte)
  return `data:audio/wav;base64,${btoa(binary)}`
}

function ascii(view: DataView, at: number, text: string): void {
  for (let index = 0; index < text.length; index += 1) {
    view.setUint8(at + index, text.charCodeAt(index))
  }
}
