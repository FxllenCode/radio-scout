/**
 * Real, decodable WAV audio as a `data:` URL — what the browser-mode layer (#34)
 * plays instead of a mocked media element.
 *
 * A Call's audio is a URL to the server in production, and the jsdom suite mocks
 * that boundary with MSW. Here the point is the *decoder*, so the bytes have to
 * be real and they may as well arrive without a network: a `data:` URL is a
 * media resource a browser loads, decodes and reports a duration for, exactly
 * like a fetched one.
 *
 * Deliberately not `lib/silence.ts`'s encoder, though the container is the same
 * shape — and the duplication is the point rather than an oversight. That
 * encoder is **production code under test here**: a fixture built from it could
 * not falsify it, because a header it wrote wrongly would be read back by the
 * same wrong arithmetic. (It is also fixed at one length and one pitch, so a
 * test using it could not tell the keep-alive loop apart from a Call — but that
 * is the lesser reason.)
 */

/** Sample rate. Small: nothing here is listened to, and a shorter file loads
 *  sooner, which is time every test in this layer spends waiting. */
const HZ = 8_000

/**
 * A mono 16-bit PCM WAV of `seconds`, as a `data:` URL.
 *
 * `pitch` is what makes two of these distinguishable to a human ear if anyone
 * ever listens; to a test they are told apart by their URLs, which differ
 * because their samples do.
 */
export function wavDataUrl(seconds = 0.5, pitch = 440): string {
  const samples = Math.round(HZ * seconds)
  const bytes = new Uint8Array(44 + samples * 2)
  const view = new DataView(bytes.buffer)

  ascii(view, 0, 'RIFF')
  view.setUint32(4, 36 + samples * 2, true)
  ascii(view, 8, 'WAVE')
  ascii(view, 12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true) // uncompressed PCM
  view.setUint16(22, 1, true) // mono
  view.setUint32(24, HZ, true)
  view.setUint32(28, HZ * 2, true) // byte rate
  view.setUint16(32, 2, true) // block align
  view.setUint16(34, 16, true) // bits per sample
  ascii(view, 36, 'data')
  view.setUint32(40, samples * 2, true)

  // Quiet, but not silent: a real signal a decoder has to do something with.
  for (let sample = 0; sample < samples; sample += 1) {
    const value = Math.sin((2 * Math.PI * pitch * sample) / HZ) * 2_000
    view.setInt16(44 + sample * 2, value, true)
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
