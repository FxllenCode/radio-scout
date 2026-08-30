import { act, waitFor } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'

import {
  engageCatchup,
  received,
  selectLiveCall,
  selectQueue,
  stopCatchup,
} from '@/store/live'
import { makeStore } from '@/store/store'
import { ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

const call = (id: number): Call => ({
  id,
  systemRef: 11,
  talkgroupRef: 100,
  durationMs: 12_000,
  audioUrl: `/api/call/${id}/audio`,
})

/** Every `ids=` the app asked about, in order — the whole of what this hook
 *  does that is visible from outside. */
function watchRequests() {
  const asked: string[] = []
  server.use(
    http.get(`${ORIGIN}/api/calls/quiet`, ({ request }) => {
      asked.push(new URL(request.url).searchParams.get('ids') ?? '')
      return HttpResponse.json({ '2': [[1500, 4500]] })
    }),
  )
  return asked
}

/** The app, with `count` Calls arrived — the first playing, the rest waiting. */
function listening(count: number) {
  const store = makeStore()
  renderApp('/', store)
  act(() => {
    for (let id = 1; id <= count; id += 1) store.dispatch(received(call(id), id))
  })
  return store
}

describe('pulling the quiet spans a queued Call is missing (#59, spec US 23)', () => {
  /**
   * **A Listener who is caught up never sends this request**, which is the whole
   * reason it is a pull. A span is useless to somebody who is not behind, so a
   * frame per Call carrying one would be traffic every Pi pays for and almost
   * nobody spends.
   */
  it('asks for nothing until Catch-up is engaged', async () => {
    const asked = watchRequests()
    const store = listening(4)

    await waitFor(() => expect(selectQueue(store.getState())).toHaveLength(3))
    expect(asked).toEqual([])

    act(() => {
      store.dispatch(engageCatchup())
    })

    await waitFor(() => expect(asked).toEqual(['1,2,3,4']))
  })

  /**
   * **The Call already playing is in the window.** Engaging Catch-up is
   * something a Listener does *while* a Call is on the air, and that Call has
   * already left the queue — so asking about the queue alone would leave the
   * first Call of every drain untrimmed, which is the one they watch to decide
   * whether this works at all.
   */
  it('asks about the Call already playing, and writes its spans on', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls/quiet`, () =>
        HttpResponse.json({ '1': [[500, 2000]] }),
      ),
    )
    const store = listening(2)

    act(() => {
      store.dispatch(engageCatchup())
    })

    await waitFor(() =>
      expect(selectLiveCall(store.getState())?.quiet).toEqual([[500, 2000]]),
    )
  })

  /**
   * The answer is written onto the Calls themselves, so everything downstream
   * reads one field whichever way the Call arrived — and the Calls the server
   * had nothing to say about are written too, as an empty list.
   */
  it('writes what came back onto the waiting Calls, and never asks twice', async () => {
    const asked = watchRequests()
    const store = listening(3)
    act(() => {
      store.dispatch(engageCatchup())
    })

    await waitFor(() =>
      expect(selectQueue(store.getState())[0].quiet).toEqual([[1500, 4500]]),
    )
    // Absent from the reply — "looked up, no gaps" — and therefore never asked
    // about again, which is what stops every ordinary Call being re-requested
    // on every pass forever.
    expect(selectQueue(store.getState())[1].quiet).toEqual([])
    expect(asked).toEqual(['1,2,3'])

    act(() => {
      store.dispatch(stopCatchup())
      store.dispatch(engageCatchup())
    })
    await waitFor(() => expect(asked).toEqual(['1,2,3']))
  })

  /** Only the head of the queue: asking about all hundred would fetch spans for
   *  Calls a Listener will drop, jump past, or never reach. */
  it('asks only about the window it is about to play', async () => {
    const asked = watchRequests()
    const store = listening(14)

    act(() => {
      store.dispatch(engageCatchup())
    })

    await waitFor(() => expect(asked).toEqual(['1,2,3,4,5,6,7,8,9']))
  })
})
