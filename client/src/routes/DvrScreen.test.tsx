import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { axe } from 'vitest-axe'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  ARCHIVE,
  FILTER_OPTIONS,
  ORIGIN,
  activitySeries,
  archivePage,
} from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'
import { CATCHUP_RATE } from '@/lib/catchup'
import { dateTimeLocalToMs, msToDateTimeLocal } from '@/lib/archive'
import { makeStore } from '@/store/store'
import { chooseEverything, chooseTalkgroups } from '@/store/live'

/** Every `/api/calls` query string the screen sent, in order. */
let searches: string[] = []
/** Every `/api/calls/activity` query string — the timeline's own requests. */
let aggregates: string[] = []
/** Every audio path anything asked for, by the player or by a prefetch. */
let audioRequests: string[] = []

beforeEach(() => {
  searches = []
  aggregates = []
  audioRequests = []
  server.use(
    http.get(`${ORIGIN}/api/calls/activity`, ({ request }) => {
      const url = new URL(request.url)
      aggregates.push(url.search)
      return HttpResponse.json(activitySeries(url))
    }),
    http.get(`${ORIGIN}/api/calls`, ({ request }) => {
      const url = new URL(request.url)
      searches.push(url.search)
      return HttpResponse.json(archivePage(url))
    }),
    http.get(`${ORIGIN}/api/call/:id/audio`, ({ request }) => {
      audioRequests.push(new URL(request.url).pathname)
      return new HttpResponse('audio-bytes', {
        headers: { 'content-type': 'audio/mpeg' },
      })
    }),
  )
})

/** The whole archive's span, so a DVR opened over it holds every fixture Call.
 *  Oldest first, because that is the order a DVR plays them in. */
const OLDEST = ARCHIVE.at(-1)!.timestamp!
const NEWEST = ARCHIVE[0].timestamp!
const WHOLE_ARCHIVE = `after=${OLDEST}&before=${NEWEST + 1}`

const lastSearch = () => new URLSearchParams(searches.at(-1))
const timeline = () => screen.getByTestId('density-ribbon')
/** The DVR's own transport, scoped: the shell's mini-player carries the same
 *  controls on every screen but the Live one, so an unscoped `Pause` is two. */
const transport = () =>
  within(screen.getByRole('region', { name: 'DVR transport' }))
const player = () => document.querySelector('audio') as HTMLAudioElement

describe('the DVR (#63, spec US 39)', () => {
  it('opens on the range and scope a link names', async () => {
    renderApp(`/dvr?sel=0_100.1&${WHOLE_ARCHIVE}`)

    await waitFor(() => expect(aggregates.length).toBeGreaterThan(0))
    const asked = new URLSearchParams(aggregates.at(-1))
    expect(asked.get('sel')).toBe('0_100.1')
    expect(asked.get('after')).toBe(String(OLDEST))
    expect(asked.get('before')).toBe(String(NEWEST + 1))
  })

  it('opens on the scanner the Listener arrived with when a link names no scope', async () => {
    const store = makeStore()
    act(() => {
      store.dispatch(chooseEverything(false))
      store.dispatch(
        chooseTalkgroups({
          keys: [{ systemRef: 100, talkgroupRef: 1 }],
          on: true,
        }),
      )
    })

    renderApp(`/dvr?${WHOLE_ARCHIVE}`, store)

    await waitFor(() => expect(aggregates.length).toBeGreaterThan(0))
    // Their own scanner, spelled as the matrix — never a list of what is on,
    // so a Talkgroup nobody has decided on inherits the same default here as
    // it does on the feed.
    expect(new URLSearchParams(aggregates.at(-1)).get('sel')).toBe('0_100.1')
  })

  it('walks the range forwards, whatever the archive is sorted by elsewhere', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    await user.click(await screen.findByLabelText('Play from here'))

    await waitFor(() => expect(lastSearch().get('sort')).toBe('oldest'))
    expect(lastSearch().get('after')).toBe(String(OLDEST))
    // A DVR that played backwards would be a search result (CONTEXT.md), so
    // the ordering above *is* the assertion; what plays is whatever the archive
    // answered with first.
    await waitFor(() =>
      expect(player()).toHaveAttribute('src', ARCHIVE.at(-1)!.audioUrl),
    )
  })

  /** Keeping what the rewind turned up (#66, spec US 37) — the gesture this
   *  screen exists for, since going back to 2am means something happened. */
  it('stars the Call it is playing, and offers the control before it plays one', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    // A transport that lost a button between Calls would be worse than one that
    // greys it, so the control is there from the start with nothing under it.
    expect(await transport().findByRole('button', { name: /^Star / })).toBeDisabled()

    await user.click(await screen.findByLabelText('Play from here'))
    await waitFor(() =>
      expect(player()).toHaveAttribute('src', ARCHIVE.at(-1)!.audioUrl),
    )
    await user.click(transport().getByRole('button', { name: /^Star / }))

    expect(
      await transport().findByRole('button', { name: /^Unstar / }),
    ).toHaveAttribute('aria-pressed', 'true')
  })

  it('warms the Call behind the one playing, which is what gapless costs', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    await user.click(await screen.findByLabelText('Play from here'))

    // #14's prefetch, reached through the Run like every other screen's.
    await waitFor(() =>
      expect(audioRequests).toContain(ARCHIVE.at(-2)!.audioUrl),
    )
  })

  it('re-anchors the Run where the Listener scrubs to, rather than merely paging', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    await user.click(await screen.findByLabelText('Play from here'))
    await waitFor(() => expect(lastSearch().get('after')).toBe(String(OLDEST)))

    // Keyboard scrubbing, which is the ARIA slider contract the ribbon already
    // answers — and the only way to drive a drag deterministically in jsdom.
    timeline().focus()
    await user.keyboard('{End}')

    await waitFor(() =>
      expect(Number(lastSearch().get('after'))).toBeGreaterThan(OLDEST),
    )
    // The search moved; the *range* did not, or the timeline would shrink under
    // the thumb as the Listener dragged along it.
    expect(new URLSearchParams(aggregates.at(-1)).get('after')).toBe(
      String(OLDEST),
    )
  })

  it('does not start playing merely because the Listener pointed at a time', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await screen.findByTestId('density-ribbon')

    timeline().focus()
    await user.keyboard('{End}')

    // Choosing where to start is not starting.
    expect(player()).not.toHaveAttribute('src')
    expect(searches).toHaveLength(0)
  })

  it('hurries through the run on both levers, and says which state it is in', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await user.click(await screen.findByLabelText('Play from here'))
    await waitFor(() => expect(player()).toHaveAttribute('src'))

    const faster = screen.getByRole('button', { name: /skip quiet/i })
    expect(faster).toHaveAttribute('aria-pressed', 'false')

    await user.click(faster)

    expect(faster).toHaveAttribute('aria-pressed', 'true')
    await waitFor(() => expect(player().playbackRate).toBe(CATCHUP_RATE))
    // ...and speech stays intelligible, which is the acceptance criterion and
    // not a preference.
    expect(player().preservesPitch).toBe(true)
  })

  it('seeks within the Call on the element', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await user.click(await screen.findByLabelText('Play from here'))
    await waitFor(() => expect(player()).toHaveAttribute('src'))

    // The element only learns a duration once it has read the header.
    for (const [name, value] of Object.entries({ currentTime: 0, duration: 30 })) {
      Object.defineProperty(player(), name, {
        value,
        configurable: true,
        writable: true,
      })
    }
    act(() => {
      player().dispatchEvent(new Event('loadedmetadata'))
    })

    const seek = screen.getByLabelText('Seek within this call')
    act(() => {
      // `fireEvent`-style, because a range input is dragged rather than typed.
      Object.getOwnPropertyDescriptor(
        window.HTMLInputElement.prototype,
        'value',
      )!.set!.call(seek, '12')
      seek.dispatchEvent(new Event('change', { bubbles: true }))
    })

    expect(player().currentTime).toBe(12)
  })

  it('says so when a stretch has nothing in it to play', async () => {
    const user = userEvent.setup()
    server.use(
      http.get(`${ORIGIN}/api/calls`, () =>
        HttpResponse.json({
          results: [],
          count: 0,
          limit: 50,
          offset: 0,
          hasMore: false,
        }),
      ),
    )
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    await user.click(await screen.findByLabelText('Play from here'))

    expect(await screen.findByRole('status')).toHaveTextContent(
      /nothing to play/i,
    )
  })

  it('says so when the archive cannot be read at all', async () => {
    const user = userEvent.setup()
    server.use(
      http.get(`${ORIGIN}/api/calls`, () => HttpResponse.error()),
    )
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    await user.click(await screen.findByLabelText('Play from here'))

    expect(await screen.findByRole('status')).toHaveTextContent(
      /could not read the archive/i,
    )
  })

  it('is reachable from the search that was already filtered', async () => {
    const user = userEvent.setup()
    renderApp('/search?system=100&talkgroup=1')

    await user.click(
      await screen.findByRole('link', { name: /open these results in the dvr/i }),
    )

    // The channel that was filtered, as the one-entry Selection it is.
    await waitFor(() => expect(aggregates.length).toBeGreaterThan(0))
    expect(new URLSearchParams(aggregates.at(-1)).get('sel')).toBe('0_100.1')
  })

  it('has no accessibility violations', async () => {
    const { container } = renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await screen.findByTestId('density-ribbon')

    expect(await axe(container)).toHaveNoViolations()
  })

  it('changes scope without leaving the range behind', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?sel=0_100.1&${WHOLE_ARCHIVE}`)

    // The picker fills in from `GET /api/calls/filters`, so wait for it.
    await screen.findByRole('option', { name: 'Alpha Law' })
    await user.selectOptions(screen.getByLabelText('Scope'), '100:2')

    await waitFor(() =>
      expect(new URLSearchParams(aggregates.at(-1)).get('sel')).toBe('0_100.2'),
    )
    expect(new URLSearchParams(aggregates.at(-1)).get('after')).toBe(
      String(OLDEST),
    )
  })

  it('says so, rather than claiming to be yours, when a link carries somebody else\'s scanner', async () => {
    // The picker is the control that says what is playing, so labelling a
    // stranger's Selection "My scanner" would be plainly false.
    renderApp(`/dvr?sel=1_100.-5&${WHOLE_ARCHIVE}`)

    const scope = (await screen.findByLabelText('Scope')) as HTMLSelectElement
    expect(scope.selectedOptions[0]).toHaveTextContent('A shared selection')
    // ...and it is a description, never a thing to choose.
    expect(scope.selectedOptions[0]).toBeDisabled()
  })

  it('goes back to the Listener own scanner', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?sel=0_100.1&${WHOLE_ARCHIVE}`)
    await screen.findByRole('option', { name: 'Alpha Law' })

    await user.selectOptions(screen.getByLabelText('Scope'), '')

    // The default scanner in a fresh store is everything.
    await waitFor(() =>
      expect(new URLSearchParams(aggregates.at(-1)).get('sel')).toBe('1'),
    )
  })

  it('walks the Run with the transport, and stops', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await user.click(await screen.findByLabelText('Play from here'))
    await waitFor(() =>
      expect(player()).toHaveAttribute('src', ARCHIVE.at(-1)!.audioUrl),
    )

    await user.click(transport().getByLabelText('Next call'))
    await waitFor(() =>
      expect(player()).toHaveAttribute('src', ARCHIVE.at(-2)!.audioUrl),
    )

    await user.click(transport().getByLabelText('Previous call'))
    await waitFor(() =>
      expect(player()).toHaveAttribute('src', ARCHIVE.at(-1)!.audioUrl),
    )

    // The Call keeps its place rather than being stopped (spec US 15).
    await user.click(transport().getByLabelText('Pause'))
    await user.click(await transport().findByLabelText('Resume'))

    await user.click(transport().getByLabelText('Stop'))
    await waitFor(() =>
      expect(transport().getByText('Nothing playing')).toBeInTheDocument(),
    )
  })

  it('takes a range from a preset, and from a date typed by hand', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await screen.findByTestId('density-ribbon')

    await user.click(screen.getByRole('button', { name: 'Last 7 days' }))
    await waitFor(() =>
      expect(Number(new URLSearchParams(aggregates.at(-1)).get('after'))).not.toBe(
        OLDEST,
      ),
    )

    const from = screen.getByLabelText('From') as HTMLInputElement
    await user.clear(from)
    await user.type(from, msToDateTimeLocal(OLDEST))

    await waitFor(() =>
      expect(new URLSearchParams(aggregates.at(-1)).get('after')).toBe(
        String(OLDEST),
      ),
    )
  })

  it('never submits itself: the range applies as it is chosen', async () => {
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await screen.findByLabelText('To')

    // A bare `<button>` inside a form submits it — the trap #49 was caught by
    // — and a submit here would navigate away from the Run mid-play.
    // `fireEvent.submit` returns false when the default was prevented.
    expect(
      fireEvent.submit(
        screen.getByRole('form', { name: 'DVR scope and range' }),
      ),
    ).toBe(false)
  })

  it('takes an upper bound typed by hand', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    const to = (await screen.findByLabelText('To')) as HTMLInputElement

    const typed = msToDateTimeLocal(NEWEST)
    await user.clear(to)
    await user.type(to, typed)

    await waitFor(() =>
      expect(new URLSearchParams(aggregates.at(-1)).get('before')).toBe(
        // A `datetime-local` has no seconds, so what comes back is the minute
        // the Listener typed and not the instant the fixture was built at.
        String(dateTimeLocalToMs(typed)),
      ),
    )
  })

  it('shares the view it is on, range and scope together', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?sel=0_100.1&${WHOLE_ARCHIVE}`)
    const written: string[] = []
    Object.defineProperty(navigator, 'clipboard', {
      value: {
        writeText: vi.fn((text: string) => {
          written.push(text)
          return Promise.resolve()
        }),
      },
      configurable: true,
    })

    await user.click(await screen.findByLabelText('Copy link to this DVR'))

    await waitFor(() => expect(written).toHaveLength(1))
    const shared = new URL(written[0])
    expect(shared.pathname).toBe('/dvr')
    expect(shared.searchParams.get('sel')).toBe('0_100.1')
    expect(shared.searchParams.get('after')).toBe(String(OLDEST))
  })

  it('offers a channel nobody has named under its Ref', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls/filters`, () =>
        HttpResponse.json({
          ...FILTER_OPTIONS,
          talkgroups: [{ systemRef: 100, ref: 42 }],
        }),
      ),
    )
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    // A Ref is what auto-populate (#8) mints before anybody curates a label,
    // and a blank row in the picker would be unchooseable.
    expect(await screen.findByRole('option', { name: '42' })).toBeInTheDocument()
  })

  it('offers no speed control until there is a Run to hurry through', async () => {
    // The lever belongs to the Run: the reducer wrapper clears it whenever
    // there is no Run, so a button offered before Play would set a flag that is
    // cleared in the same dispatch — a control that visibly does nothing.
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)

    expect(
      await screen.findByRole('button', { name: /skip quiet/i }),
    ).toBeDisabled()
  })

  it('stops rather than limping on when a Listener rewinds into an empty stretch', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    await user.click(await screen.findByLabelText('Play from here'))
    await waitFor(() => expect(player()).toHaveAttribute('src'))

    // Nothing at all from here on.
    server.use(
      http.get(`${ORIGIN}/api/calls`, () =>
        HttpResponse.json({
          results: [],
          count: 0,
          limit: 50,
          offset: 0,
          hasMore: false,
        }),
      ),
    )
    timeline().focus()
    await user.keyboard('{End}')

    // The scrub told the Run its search had changed, which disarms it — so
    // leaving it alone would play out the page it happens to hold and then
    // stop, minutes later, looking like the archive having run out. The
    // Listener asked to be somewhere with nothing in it; say so, and stop.
    expect(await screen.findByRole('status')).toHaveTextContent(/nothing to play/i)
    await waitFor(() =>
      expect(transport().getByText('Nothing playing')).toBeInTheDocument(),
    )
  })

  it('keeps the range when a half-typed date runs backwards', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?${WHOLE_ARCHIVE}`)
    const to = (await screen.findByLabelText('To')) as HTMLInputElement

    // Retyping a year emits `0002`, `0020`, `0202` on the way to `2027`, each
    // of which is a range running backwards.
    await user.clear(to)
    await user.type(to, '0002-01-01T00:00')

    await waitFor(() => expect(aggregates.length).toBeGreaterThan(0))
    expect(new URLSearchParams(aggregates.at(-1)).get('after')).toBe(String(OLDEST))
    expect(new URLSearchParams(aggregates.at(-1)).get('before')).toBe(
      String(NEWEST + 1),
    )
  })
})

describe('taking a rewind away (#65, spec US 33)', () => {
  /** The whole range on the timeline, not from the playhead — a Listener who
   *  scrubbed forward before downloading must not silently get less than they
   *  chose. */
  it('exports the range, not the anchor', async () => {
    const user = userEvent.setup()
    renderApp(`/dvr?sel=0_100.1&${WHOLE_ARCHIVE}&at=${NEWEST}`)
    await screen.findByTestId('density-ribbon')

    await user.click(screen.getByRole('button', { name: 'Export these results' }))

    const stitched = await screen.findByRole('link', { name: /one stitched file/i })
    const href = new URLSearchParams(stitched.getAttribute('href')!.split('?')[1])
    expect(href.get('after')).toBe(String(OLDEST))
    expect(href.get('before')).toBe(String(NEWEST + 1))
    expect(href.get('sel')).toBe('0_100.1')
  })
})
