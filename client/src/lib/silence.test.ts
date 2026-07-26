import { describe, expect, it } from 'vitest'

import { KEEP_ALIVE_HZ, keepAliveLoopUrl } from './silence'

/** The bytes behind the data URL. */
function decode(url: string): DataView {
  const [header, base64] = url.split(',')
  expect(header).toBe('data:audio/wav;base64')
  return new DataView(
    Uint8Array.from(atob(base64), (c) => c.charCodeAt(0)).buffer,
  )
}

const text = (view: DataView, at: number) =>
  String.fromCharCode(...new Uint8Array(view.buffer, at, 4))

describe('the keep-alive loop', () => {
  // Checked against the RIFF/WAVE layout by hand rather than against our own
  // encoder: a header this browser won't parse is a keep-alive that never
  // plays, and a keep-alive that never plays is a listener whose phone goes to
  // sleep mid-shift.
  it('is a WAV a browser will actually decode', () => {
    const wav = decode(keepAliveLoopUrl())

    expect(text(wav, 0)).toBe('RIFF')
    expect(text(wav, 8)).toBe('WAVE')
    expect(text(wav, 12)).toBe('fmt ')
    expect(wav.getUint32(16, true)).toBe(16) // PCM fmt chunk length
    expect(wav.getUint16(20, true)).toBe(1) // uncompressed PCM
    expect(wav.getUint16(22, true)).toBe(1) // mono
    expect(wav.getUint32(24, true)).toBe(KEEP_ALIVE_HZ)
    expect(wav.getUint32(28, true)).toBe(KEEP_ALIVE_HZ * 2) // byte rate
    expect(wav.getUint16(32, true)).toBe(2) // block align
    expect(wav.getUint16(34, true)).toBe(16) // bits per sample
    expect(text(wav, 36)).toBe('data')

    // One second at 8 kHz, 2 bytes a sample: 16000 bytes of audio, a 44-byte
    // header, and a RIFF size that counts everything after its own 8 bytes.
    // Written out rather than derived, so a mistake in the encoder's own
    // arithmetic can't agree with the assertion.
    expect(wav.getUint32(40, true)).toBe(16_000) // data chunk length
    expect(wav.getUint32(4, true)).toBe(16_036) // RIFF chunk length
    expect(wav.byteLength).toBe(16_044)
  })

  // The load-bearing property. WebKit's `computeCanProduceAudio()` never looks
  // at sample values, so digital silence would do — but nothing below WebKit
  // is documented, and a ±1-LSB square is inaudible on any hardware while
  // being unmistakably not-silence to anything that looks.
  it('carries real, inaudible audio rather than digital silence', () => {
    const wav = decode(keepAliveLoopUrl())

    const amplitudes = new Set<number>()
    for (let at = 44; at < wav.byteLength; at += 2) {
      amplitudes.add(wav.getInt16(at, true))
    }

    expect([...amplitudes].sort()).toEqual([-1, 1])
  })
})
