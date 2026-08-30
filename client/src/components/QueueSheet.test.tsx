import { act, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { axe } from 'vitest-axe'

import {
  received,
  selectIsCatchingUp,
  selectLiveCall,
  selectMissed,
  selectQueue,
  selectQueueDepth,
  togglePriority,
} from '@/store/live'
import { makeStore } from '@/store/store'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

function call(id: number, talkgroupRef = 54241): Call {
  return {
    id,
    systemRef: 11,
    systemLabel: 'Fulton County',
    talkgroupRef,
    talkgroupLabel: `Talkgroup ${talkgroupRef}`,
    timestamp: Date.parse('2026-07-25T14:32:05'),
    audioUrl: `/api/call/${id}/audio`,
  }
}

/** The Live screen with `calls` arrived — the first playing, the rest waiting. */
function listening(...calls: Call[]) {
  const store = makeStore()
  const view = renderApp('/', store)
  act(() => {
    for (const one of calls) store.dispatch(received(one, one.id))
  })
  return { ...view, store }
}

type Listening = ReturnType<typeof listening>

const open = async (user: ReturnType<typeof userEvent.setup>) =>
  user.click(screen.getByRole('button', { name: /Queued calls/ }))

const sheet = () => screen.getByRole('dialog')
const rows = () =>
  within(screen.getByRole('list', { name: 'Queued calls' })).getAllByRole('listitem')

const queuedIds = (store: Listening['store']) =>
  selectQueue(store.getState()).map((one) => one.id)

describe('the queue sheet (#58, spec US 24)', () => {
  /** The `Q` readout has been a number since #11 and this is what it becomes. */
  it('opens from the queue counter and lists what is waiting', async () => {
    const user = userEvent.setup()
    listening(call(1), call(2, 100), call(3, 200))

    await open(user)

    expect(sheet()).toHaveAccessibleName('Queue — 2 waiting')
    expect(rows()).toHaveLength(2)
    expect(within(rows()[0]).getByText('Talkgroup 100')).toBeInTheDocument()
  })

  /**
   * spec US 24 asks to "see... what's waiting", and *nothing is waiting* is an
   * answer — so an empty queue opens the sheet and the sheet says so, rather
   * than the counter going dead at exactly the moment a Listener wonders
   * whether they are caught up.
   */
  it('opens on an empty queue and says the feed is caught up', async () => {
    const user = userEvent.setup()
    listening(call(1))

    await open(user)

    expect(screen.getByText(/Nothing waiting/)).toBeInTheDocument()
  })

  /**
   * What it *is* gated on is the feed. **Feed off** and **Playback mode** empty
   * the queue by construction, so a counter offering to open one there would be
   * a control that looks live and does nothing — the thing #88's own tests
   * exist to catch.
   */
  it('is out of reach with the feed off', async () => {
    const user = userEvent.setup()
    listening(call(1), call(2, 100))

    await user.click(screen.getByRole('button', { name: 'Live feed' }))

    expect(screen.getByRole('button', { name: /Queued calls/ })).toBeDisabled()
  })

  it('plays a waiting Call now, and closes', async () => {
    const user = userEvent.setup()
    const { store } = listening(call(1), call(2, 100), call(3, 200))
    await open(user)

    await user.click(screen.getByRole('button', { name: 'Play Talkgroup 200 now' }))

    expect(selectLiveCall(store.getState())?.id).toBe(3)
    expect(queuedIds(store)).toEqual([2])
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  /** Pruning is a run of taps, so the sheet stays up — closing after each one
   *  would make clearing four Calls cost eight taps. */
  it('drops a waiting Call and stays open', async () => {
    const user = userEvent.setup()
    const { store } = listening(call(1), call(2, 100), call(3, 200))
    await open(user)

    await user.click(screen.getByRole('button', { name: 'Drop Talkgroup 100' }))

    expect(queuedIds(store)).toEqual([3])
    expect(rows()).toHaveLength(1)
    expect(screen.getByRole('dialog')).toBeInTheDocument()
  })

  /** They read the row and let it go: `missed` is traffic the Listener wanted
   *  and did not get, which this is not. */
  it('counts a dropped Call as nothing', async () => {
    const user = userEvent.setup()
    const { store } = listening(call(1), call(2, 100))
    await open(user)

    await user.click(screen.getByRole('button', { name: 'Drop Talkgroup 100' }))

    expect(selectMissed(store.getState())).toBe(0)
  })

  it('jumps to the newest, admitting what that cost', async () => {
    const user = userEvent.setup()
    const { store } = listening(call(1), call(2, 100), call(3, 200), call(4, 300))
    await open(user)

    await user.click(screen.getByRole('button', { name: /Jump to newest/ }))

    expect(selectLiveCall(store.getState())?.id).toBe(4)
    expect(selectQueueDepth(store.getState())).toBe(0)
    expect(selectMissed(store.getState())).toBe(2)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('says how many the jump would give up before it is pressed', async () => {
    const user = userEvent.setup()
    listening(call(1), call(2, 100), call(3, 200), call(4, 300))

    await open(user)

    expect(
      screen.getByRole('button', { name: /Jump to newest.*2 counted missed/ }),
    ).toBeInTheDocument()
  })

  /** One Call waiting is not a backlog, and "0 counted missed" beside a button
   *  is a number that says nothing. */
  it('says nothing about the cost when the jump gives up nothing', async () => {
    const user = userEvent.setup()
    listening(call(1), call(2, 100))

    await open(user)

    expect(screen.getByRole('button', { name: 'Jump to newest' })).toBeInTheDocument()
  })

  /**
   * The list re-orders under the Listener's thumb whenever a **Priority** Call
   * arrives — so every control names a Call id and `key={call.id}` moves the
   * row's node rather than rewriting it. A control keyed on *position* would
   * drop whichever Call had slid into that slot.
   */
  it('acts on the Call it names after the queue re-orders underneath it', async () => {
    const user = userEvent.setup()
    const { store } = listening(call(1), call(2, 100), call(3, 200))
    await open(user)
    expect(rows()).toHaveLength(2)

    // A Priority Call arrives and takes the head of the queue.
    act(() => {
      store.dispatch(togglePriority('11:900'))
      store.dispatch(received(call(4, 900), 4))
    })
    expect(queuedIds(store)).toEqual([4, 2, 3])

    await user.click(screen.getByRole('button', { name: 'Drop Talkgroup 100' }))

    expect(queuedIds(store)).toEqual([4, 3])
  })

  /** Emptying the queue from inside the sheet leaves it up and saying so,
   *  rather than closing on a Listener mid-prune. */
  it('says the feed is caught up once the last Call goes', async () => {
    const user = userEvent.setup()
    listening(call(1), call(2, 100))
    await open(user)

    await user.click(screen.getByRole('button', { name: 'Drop Talkgroup 100' }))

    expect(screen.getByText(/Nothing waiting/)).toBeInTheDocument()
  })

  it('has no accessibility violations', async () => {
    const user = userEvent.setup()
    const { container } = listening(call(1), call(2, 100))

    await open(user)

    expect(await axe(container)).toHaveNoViolations()
  })
})

describe('Catch-up from the queue sheet (#59, spec US 23)', () => {
  /** A Call the shape a Trunk Recorder file really is: twelve seconds, of which
   *  nine is nobody talking. */
  const long = (id: number, talkgroupRef = 54241): Call => ({
    ...call(id, talkgroupRef),
    durationMs: 12_000,
    quiet: [
      [1500, 4500],
      [6000, 12_000],
    ],
  })

  const control = () => screen.getByRole('button', { name: /Catch up|Catching up/ })

  /**
   * **The offer says what it is worth**, because a bare "4:20" says nothing
   * about whether the button is worth pressing — and this is the sheet where
   * the other way out of a backlog is *give it up and lose 39 Calls*.
   */
  it('offers to catch up, and says what it would save', async () => {
    const user = userEvent.setup()
    listening(long(1), long(2, 100), long(3, 200))
    await open(user)

    // Two waiting: 24 seconds as recorded, 4 with the gaps out at 1.5×.
    expect(control()).toHaveTextContent('0:04 instead of 0:24')
  })

  it('engages, counts down, and can be stopped', async () => {
    const user = userEvent.setup()
    const { store } = listening(long(1), long(2, 100), long(3, 200))
    await open(user)

    await user.click(control())

    expect(selectIsCatchingUp(store.getState())).toBe(true)
    expect(control()).toHaveTextContent('Catching up at 1.5×')
    expect(control()).toHaveTextContent('0:04 left')
    // The sheet stays up: a Listener watching the estimate fall is watching
    // this work, where playing and jumping are done with the sheet.
    expect(screen.getByRole('dialog')).toBeInTheDocument()

    await user.click(control())
    expect(selectIsCatchingUp(store.getState())).toBe(false)
  })

  /**
   * A countdown that reached zero with Calls still waiting would be the display
   * lying about the feed, which is what #56 was for — so a Call nobody measured
   * makes the estimate a floor and the readout says so.
   */
  it('marks the estimate as a floor when a waiting Call has no duration', async () => {
    const user = userEvent.setup()
    listening(long(1), long(2, 100), call(3, 200))
    await open(user)

    expect(control()).toHaveTextContent('0:02+ instead of 0:12+')
  })
})
