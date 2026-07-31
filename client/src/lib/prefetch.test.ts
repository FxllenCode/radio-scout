import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'

import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'

import { prefetchAudio } from './prefetch'

/** Every audio URL the prefetcher asked for, in order. */
let requested: string[] = []

beforeEach(() => {
  requested = []
  server.use(
    http.get(`${ORIGIN}/api/call/:id/audio`, ({ request }) => {
      requested.push(new URL(request.url).pathname)
      return new HttpResponse('audio-bytes', {
        headers: { 'content-type': 'audio/mpeg' },
      })
    }),
  )
})

describe('next-call prefetch', () => {
  it("fetches the call's audio, so playing it is a cache hit", async () => {
    await prefetchAudio('/api/call/2/audio')

    expect(requested).toEqual(['/api/call/2/audio'])
  })

  it.each([
    ['the call is gone', () => new HttpResponse(null, { status: 404 })],
    ['the server is down', () => HttpResponse.error()],
  ])('gives up quietly when %s', async (_case, respond) => {
    server.use(http.get(`${ORIGIN}/api/call/:id/audio`, respond))

    // A prefetch is an optimization: failing one must not surface to the
    // listener, and must not stop the Call itself from playing.
    await expect(prefetchAudio('/api/call/9/audio')).resolves.toBeUndefined()
  })

  it('asks for nothing when the Call has no audio', async () => {
    // An encrypted Call carries no `audioUrl` at all (#42, spec US 9). Passing
    // `undefined` through to `fetch` would request the page the app is on —
    // wasted bytes, and a cache entry for the wrong thing.
    await prefetchAudio(undefined)

    expect(requested).toEqual([])
  })

  it('stops downloading when the queue moves on', async () => {
    const controller = new AbortController()
    const prefetch = prefetchAudio('/api/call/2/audio', controller.signal)
    controller.abort()

    await expect(prefetch).resolves.toBeUndefined()
  })
})
