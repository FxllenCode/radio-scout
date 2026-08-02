import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { feedOffKey, selectionKey } from '@/lib/persist'
import { EVERYTHING, setTalkgroups } from '@/lib/selection'

import {
  chooseTalkgroups,
  received,
  selectFeedStatus,
  selectSelection,
  turnFeedOff,
  turnFeedOn,
} from './live'
import {
  enterLiveFeed,
  enterPlaybackMode,
  playbackActions,
  selectPlaybackMode,
} from './playback'
import { makeStore, type AppStore } from './store'

/** An in-memory `Storage` that also counts what was written, so a test can say
 *  "nothing was persisted" and mean it. */
function fakeStorage(seed: Record<string, string> = {}) {
  const map = new Map(Object.entries(seed))
  const writes: string[] = []
  const storage: Storage = {
    get length() {
      return map.size
    },
    clear: () => map.clear(),
    getItem: (key) => map.get(key) ?? null,
    key: (index) => [...map.keys()][index] ?? null,
    removeItem: (key) => void map.delete(key),
    setItem: (key, value) => {
      writes.push(value)
      map.set(key, value)
    },
  }
  return { storage, writes }
}

const NARROWED = setTalkgroups(EVERYTHING, [{ systemRef: 11, talkgroupRef: 100 }], false)

describe('makeStore', () => {
  /** Whether `globalThis.localStorage` exists at all differs by Node version —
   *  and two of these tests deliberately take the *default* storage rather than
   *  passing one. Left ambient, they assert something different on a laptop than
   *  on a runner, which is how CI found `{ storage: undefined }` silently
   *  falling back to the browser's. Pinned, they assert the same thing
   *  everywhere. */
  let ambient: ReturnType<typeof fakeStorage>

  beforeEach(() => {
    ambient = fakeStorage()
    vi.stubGlobal('localStorage', ambient.storage)
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('starts a listener who has never chosen on everything', () => {
    const { storage } = fakeStorage()

    const store = makeStore({ storage, namespace: 'default' })

    expect(selectSelection(store.getState())).toEqual(EVERYTHING)
  })

  it('starts from the selection this browser last made (spec US 22)', () => {
    const { storage } = fakeStorage({
      [selectionKey('default')]: JSON.stringify(NARROWED),
    })

    const store = makeStore({ storage, namespace: 'default' })

    expect(selectSelection(store.getState())).toEqual(NARROWED)
  })

  it('remembers a selection change as it happens', () => {
    const { storage } = fakeStorage()
    const store = makeStore({ storage, namespace: 'truck' })

    store.dispatch(
      chooseTalkgroups({ keys: [{ systemRef: 11, talkgroupRef: 100 }], on: false }),
    )

    expect(storage.getItem(selectionKey('truck'))).toBe(JSON.stringify(NARROWED))
    expect(storage.getItem(selectionKey('default'))).toBeNull()
  })

  /** A Call arrives every few seconds; writing local storage on each one would
   *  cost a phone battery for nothing. */
  it('writes nothing when a Call arrives', () => {
    const { storage, writes } = fakeStorage()
    const store = makeStore({ storage, namespace: 'default' })

    store.dispatch(
      received({ id: 1, systemRef: 11, talkgroupRef: 100, audioUrl: '/api/call/1/audio' }),
    )

    expect(writes).toEqual([])
  })

  /** A browser with site data blocked — or a sandboxed context — still gets a
   *  working scanner, just not a remembered one.
   *
   *  Saying so out loud (`storage: undefined`) has to *mean* it. It used to fall
   *  through to a destructuring default and quietly take the browser's storage
   *  instead, so this test asserted nothing about the case it names and, worse,
   *  persisted a narrowed selection that the next test then read back. Hence the
   *  second assertion: unremembered means nothing was written anywhere. */
  it('runs unremembered when the browser has no storage', () => {
    const store = makeStore({ storage: undefined, namespace: 'default' })

    store.dispatch(
      chooseTalkgroups({ keys: [{ systemRef: 11, talkgroupRef: 100 }], on: false }),
    )

    expect(selectSelection(store.getState())).toEqual(NARROWED)
    expect(ambient.writes).toEqual([])
  })

  it('defaults to this browser and this tab’s scanner', () => {
    expect(selectSelection(makeStore().getState())).toEqual(EVERYTHING)
  })

  /** Feed off is remembered beside the Selection (#80). A listener who switched
   *  the feed off and reloaded must not be blasted with audio for having done
   *  so — the choice outlives the tab, exactly as their Selection does. */
  describe('the feed-off choice (#80)', () => {
    /** A store built here has never opened a socket, so "the feed is on" reads
     *  as **Feed down** rather than live (#88). What is being remembered is
     *  whether the Listener switched it **off**, which is the one arm of the
     *  derived status that persists. */
    const isOff = (store: AppStore) => selectFeedStatus(store.getState()) === 'off'

    it('starts on, so a Listener who never touched it hears the feed', () => {
      const { storage } = fakeStorage()

      const store = makeStore({ storage, namespace: 'default' })

      expect(isOff(store)).toBe(false)
    })

    it('remembers being switched off, and being switched back on', () => {
      const { storage } = fakeStorage()
      const store = makeStore({ storage, namespace: 'truck' })

      store.dispatch(turnFeedOff())
      expect(storage.getItem(feedOffKey('truck'))).toBe('true')

      store.dispatch(turnFeedOn())
      expect(storage.getItem(feedOffKey('truck'))).toBe('false')
    })

    it('starts off when that is what this browser last chose', () => {
      const { storage } = fakeStorage({ [feedOffKey('default')]: 'true' })

      const store = makeStore({ storage, namespace: 'default' })

      expect(isOff(store)).toBe(true)
    })

    /** Two Profiles in one browser (`?id=`) are two independent choices, the
     *  same way their Selections are. */
    it('is remembered per Profile', () => {
      const { storage } = fakeStorage({ [feedOffKey('truck')]: 'true' })

      expect(
        isOff(makeStore({ storage, namespace: 'default' })),
      ).toBe(false)
      expect(
        isOff(makeStore({ storage, namespace: 'truck' })),
      ).toBe(true)
    })

    /** A hand-edited or half-written value costs the listener nothing: the feed
     *  comes up on, which is the state the app is for. */
    it('ignores a stored value that is not a boolean', () => {
      const { storage } = fakeStorage({ [feedOffKey('default')]: 'maybe' })

      const store = makeStore({ storage, namespace: 'default' })

      expect(isOff(store)).toBe(false)
    })

    it('runs unremembered when the browser has no storage', () => {
      const store = makeStore({ storage: undefined, namespace: 'default' })

      store.dispatch(turnFeedOff())

      expect(isOff(store)).toBe(true)
      expect(ambient.writes).toEqual([])
    })

    /** Playback mode is the other reason the feed goes quiet, and it is
     *  deliberately *not* remembered: a reload comes back on the live feed,
     *  which is what the app is for. */
    it('does not remember playback mode along with it', () => {
      const { storage, writes } = fakeStorage()
      const store = makeStore({ storage, namespace: 'default' })

      store.dispatch(enterPlaybackMode())

      expect(writes).toEqual([])
      expect(isOff(makeStore({ storage, namespace: 'default' }))).toBe(false)
    })
  })

  /**
   * The live slice keeps its own record of playback mode (#88), because a
   * reducer can see no other slice and the guard that refuses a Call has to be
   * in the reducer to cover every route in.
   *
   * That record is only as good as its agreement with the slice that owns the
   * mode, so both halves are pinned: that the two actions keep them in step,
   * and — the half that actually holds the line — that those two are still the
   * only actions the playback slice has. A third one that moved `mode` would
   * desync the mirror silently, and no test driving the existing two could
   * notice.
   */
  describe('the feed and playback mode, which cannot disagree', () => {
    it.each([
      { what: 'entering playback mode', actions: [enterPlaybackMode()] },
      { what: 'leaving it again', actions: [enterPlaybackMode(), enterLiveFeed()] },
      {
        what: 'switching modes twice',
        actions: [enterPlaybackMode(), enterLiveFeed(), enterPlaybackMode()],
      },
    ])('agrees after $what', ({ actions }) => {
      const store = makeStore({ storage: undefined })

      for (const action of actions) store.dispatch(action)

      const state = store.getState()
      expect(selectFeedStatus(state) === 'playback').toBe(
        selectPlaybackMode(state) === 'playback',
      )
    })

    /**
     * The structural half. `enterPlaybackMode` and `enterLiveFeed` are the only
     * two the live slice mirrors, so a new action here is a decision somebody
     * has to take: either it cannot change `mode`, or the mirror must learn it.
     * Failing this test *is* that decision being asked for.
     */
    it('has no third way to change modes for the mirror to miss', () => {
      expect(Object.keys(playbackActions).sort()).toEqual([
        'enterLiveFeed',
        'enterPlaybackMode',
        'next',
        'playResults',
        'previous',
        'stop',
      ])
    })
  })
})
