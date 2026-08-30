import { describe, expect, it } from 'vitest'

import { playingDetail, stripView, wayBack, type StripFacts } from './strip'
import { feedPlays, type FeedStatus } from './feed'
import type { Call } from '@/types'

const CALL: Call = {
  id: 7,
  systemRef: 11,
  systemLabel: 'Fulton County',
  talkgroupRef: 54241,
  talkgroupLabel: 'FD Dispatch',
  audioUrl: '/api/call/7/audio',
}

/** The live feed delivering, with nothing playing and nothing to explain. */
const QUIET_AND_WELL: StripFacts = {
  status: 'live',
  link: 'connected',
  playing: null,
  fromArchive: false,
  interrupting: false,
  position: { index: -1, total: 0 },
}

const facts = (over: Partial<StripFacts> = {}): StripFacts => ({
  ...QUIET_AND_WELL,
  ...over,
})

/**
 * The strip is the app's global truth about the transport (#56, spec US 54).
 * Every rule about *what* it says is here, as values — the component that draws
 * it states none of them, the way `feedReadout` gave the header its dot.
 */
describe('stripView', () => {
  describe('with a Call on the element', () => {
    it('names the live feed as the source', () => {
      expect(stripView(facts({ playing: CALL }))).toEqual({
        mode: 'playing',
        call: CALL,
        source: 'live',
        detail: 'Live',
      })
    })

    /** Spec US 26: the live feed is waiting underneath with its listening queue
     *  untouched, so the strip says the archive borrowed the audio rather than
     *  claiming a walk through results that is not happening. */
    it('says an archived Call is interrupting the feed', () => {
      const strip = stripView(
        facts({ playing: CALL, fromArchive: true, interrupting: true }),
      )

      expect(strip).toMatchObject({
        source: 'interrupting',
        detail: 'Interrupting live feed',
      })
    })

    /** Spec US 25: playback mode walks the whole filtered result set, so the
     *  useful fact is where in it the Listener is. */
    it('places an archive Run in its results', () => {
      const strip = stripView(
        facts({
          status: 'playback',
          playing: CALL,
          fromArchive: true,
          position: { index: 2, total: 421 },
        }),
      )

      expect(strip).toMatchObject({ source: 'archive', detail: '3 of 421' })
    })

    /**
     * A Call playing is its own explanation, so it outranks the feed's
     * standing — including **Feed off**, where an archived Call can still be
     * the audio (spec US 25). The alternative is a strip that says "Feed off"
     * over audio the Listener can hear.
     */
    it('shows the Call rather than the feed’s standing, whatever that is', () => {
      for (const status of ['live', 'off', 'playback', 'down'] as FeedStatus[]) {
        expect(stripView(facts({ status, playing: CALL }))).toMatchObject({
          mode: 'playing',
        })
      }
    })
  })

  describe('with nothing playing', () => {
    /**
     * The two silences a Listener *chose* — and the two with one tap back, which
     * is why they are the two the strip explains. `feedPlays` is the predicate,
     * so this cannot drift from the reducers' own guards.
     */
    const WAYS_BACK: { status: FeedStatus; label: string; does: string }[] = [
      { status: 'off', label: 'Turn on', does: 'feed-on' },
      { status: 'playback', label: 'Back to live', does: 'leave-playback' },
    ]

    it.each(WAYS_BACK)('offers "$label" when $status', ({ status, label, does }) => {
      expect(stripView(facts({ status }))).toMatchObject({
        mode: 'quiet',
        label,
        does,
      })
    })

    /** The word beside the dot is [`feedReadout`]'s, not a second one: a strip
     *  reading FEED OFF under a header reading PLAYBACK is exactly the lie this
     *  ticket is named for. */
    it('borrows the header’s own badge rather than inventing a word', () => {
      expect(stripView(facts({ status: 'off' }))).toMatchObject({
        badge: { label: 'FEED OFF', color: 'orange', pulse: false },
      })
    })

    /**
     * A lull on a healthy feed has nothing to say, and neither has a dropped
     * socket: **Feed down** is transient, heals itself, and has no one-tap way
     * back to offer. Distinguishing it from being offline is #78's, and saying
     * so here would put a red bar over every tab for the first seconds of every
     * visit.
     */
    const SILENT: FeedStatus[] = ['live', 'down']
    it.each(SILENT)('says nothing at all while %s', (status) => {
      expect(stripView(facts({ status, link: 'connected' }))).toBeNull()
    })
  })

  /**
   * **The two key sets are each other's proof.**
   *
   * [`stripView`] offers a way back for exactly the states [`wayBack`] knows,
   * with no second `feedPlays` guard in front of it — a guard there could never
   * change an answer, which is a branch no test can kill. What makes the
   * omission safe is this: the states with a way back are precisely the ones
   * `feedPlays` rejects. Add a fifth [`FeedStatus`] and this fails until
   * somebody decides which side it is on.
   */
  it('offers a way back out of exactly the silences the Listener chose', () => {
    const EVERY: FeedStatus[] = ['live', 'off', 'playback', 'down']

    expect(EVERY.filter((status) => wayBack(status) !== undefined)).toEqual(
      EVERY.filter((status) => !feedPlays(status)),
    )
  })
})

/** Shared with the Search screen's own now-playing bar, so the two cannot
 *  describe the same Run differently. */
describe('playingDetail', () => {
  it('counts from one, the way a Listener does', () => {
    expect(playingDetail('archive', { index: 0, total: 3 })).toBe('1 of 3')
  })
})
