import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { delay, http, HttpResponse } from 'msw'
import { axe } from 'vitest-axe'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ARCHIVE, ORIGIN, activitySeries, archivePage } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp, routerProbe } from '@/test/utils'
import { msToDateTimeLocal } from '@/lib/archive'
import { NOTICE_MS } from '@/hooks/useShareLink'

/** Every `/api/calls` query string the screen sent, in order. */
let searches: string[] = []

/** Every audio path fetched — by the player *or* by a prefetch (#14, #32).
 *  Which of the two it was is not the point: what matters is that the bytes
 *  were pulled before playback needed them. */
let audioRequests: string[] = []

beforeEach(() => {
  searches = []
  audioRequests = []
  server.use(
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

const lastSearch = () => new URLSearchParams(searches.at(-1))

/** A browser that can put a link on the clipboard, and a way to read what it
 *  was handed (#61). Called *after* `userEvent.setup()`, which installs a
 *  clipboard stub of its own that would otherwise replace this one. */
function sharesTo({ fails = false } = {}) {
  const written: string[] = []
  const writeText = vi.fn((text: string) => {
    if (fails) return Promise.reject(new Error('denied'))
    written.push(text)
    return Promise.resolve()
  })
  Object.defineProperty(navigator, 'clipboard', {
    value: { writeText },
    configurable: true,
  })
  return () => written.at(-1)
}

/** An archive of `total` Calls — 51 by default, one past a full page, so there
 *  are exactly two pages and a boundary to cross. */
function pagedArchive(total = 51) {
  const rows = Array.from({ length: total }, (_, index) => ({
    ...ARCHIVE[0],
    id: 1000 + index,
    audioUrl: `/api/call/${1000 + index}/audio`,
  }))
  server.use(
    http.get(`${ORIGIN}/api/calls`, ({ request }) => {
      const url = new URL(request.url)
      searches.push(url.search)
      return HttpResponse.json(archivePage(url, rows))
    }),
  )
  return { rows }
}

/**
 * Where the **Run** has got to, read off the Search screen's own bar.
 *
 * Scoped, because since #56 the docked mini-player says the same thing about
 * the same Run from the shell — an unscoped query finds it twice. Retried
 * rather than read once: rolling onto a page takes the bar down for a beat
 * while the Run waits for it.
 */
async function runPosition(text: string) {
  await waitFor(() =>
    expect(
      within(screen.getByRole('region', { name: 'Now playing' })).getByText(text),
    ).toBeInTheDocument(),
  )
}

/** The result rows, once the first page has landed. */
/** A row's own **Play** — the one that walks the list in the order it is
 *  sorted. Told apart from "Play forward from …" (#61), which is the same
 *  gesture anchored at that Call and pointed the other way. */
function playIn(row: HTMLElement) {
  return within(row).getByRole('button', { name: /^Play (?!forward)/ })
}

async function resultRows() {
  const list = await screen.findByRole('list', { name: 'Search results' })
  return within(list).findAllByRole('listitem')
}

/** Wait for the cascading filter options to arrive — the selects render (with
 *  only their "any" option) before `/api/calls/filters` answers. */
async function filtersLoaded() {
  await screen.findByRole('option', { name: 'Alpha' })
}

describe('SearchScreen', () => {
  it('lists archived calls with their system, tag, group, and time', async () => {
    renderApp('/search')

    const rows = await resultRows()
    expect(rows).toHaveLength(3)
    expect(within(rows[0]).getByText('Beta Dispatch')).toBeInTheDocument()
    expect(within(rows[0]).getByText('Beta · Fire · Public')).toBeInTheDocument()
    expect(within(rows[0]).getByText('2026-07-25 14:32:05')).toBeInTheDocument()
    expect(screen.getByText('1–3 of 3')).toBeInTheDocument()
  })

  it('offers a download link per call, named after the call', async () => {
    renderApp('/search')

    const download = await screen.findByRole('link', {
      name: 'Download Beta Dispatch on Beta at 2026-07-25 14:32:05',
    })
    expect(download).toHaveAttribute('href', '/api/call/3/download')
    expect(download).toHaveAttribute('download')
  })

  it('populates the cascading filters from the server, not a static config', async () => {
    renderApp('/search')

    await filtersLoaded()

    const system = screen.getByLabelText('System')
    expect(
      within(system).getByRole('option', { name: 'Alpha' }),
    ).toBeInTheDocument()
    expect(
      within(system).getByRole('option', { name: 'Beta' }),
    ).toBeInTheDocument()
    expect(
      within(screen.getByLabelText('Talkgroup')).getByRole('option', {
        name: 'Alpha Law',
      }),
    ).toBeInTheDocument()
    expect(
      within(screen.getByLabelText('Group')).getByRole('option', {
        name: 'Emergency',
      }),
    ).toBeInTheDocument()
    expect(
      within(screen.getByLabelText('Tag')).getByRole('option', { name: 'Law' }),
    ).toBeInTheDocument()
  })

  it('shows the span the archive covers, so the date picker has bounds', async () => {
    renderApp('/search')

    expect(
      await screen.findByText(
        'Archive spans 2026-07-25 14:00:00 – 2026-07-25 14:32:05',
      ),
    ).toBeInTheDocument()
  })

  it('sends each chosen filter to the server', async () => {
    const user = userEvent.setup()
    renderApp('/search')

    await filtersLoaded()

    await user.selectOptions(screen.getByLabelText('System'), '100')
    await waitFor(() => expect(lastSearch().get('system')).toBe('100'))

    await user.selectOptions(screen.getByLabelText('Tag'), 'Fire')
    await waitFor(() => expect(lastSearch().get('tag')).toBe('Fire'))

    await user.selectOptions(screen.getByLabelText('Group'), 'Emergency')
    await waitFor(() => expect(lastSearch().get('group')).toBe('Emergency'))

    await user.selectOptions(screen.getByLabelText('Sort'), 'oldest')
    await waitFor(() => expect(lastSearch().get('sort')).toBe('oldest'))
  })

  /** A Talkgroup Ref is unique only within its System, so choosing one pins
   *  both — rdio-scanner instead makes you pick the System first. */
  it('pins the system when a talkgroup is chosen', async () => {
    const user = userEvent.setup()
    renderApp('/search')

    await filtersLoaded()

    await user.selectOptions(screen.getByLabelText('Talkgroup'), '200:1')

    await waitFor(() => {
      expect(lastSearch().get('talkgroup')).toBe('1')
      expect(lastSearch().get('system')).toBe('200')
    })
  })

  it('clears the talkgroup when the system changes', async () => {
    const user = userEvent.setup()
    renderApp('/search')

    await filtersLoaded()

    await user.selectOptions(screen.getByLabelText('Talkgroup'), '100:2')
    await waitFor(() => expect(lastSearch().get('talkgroup')).toBe('2'))

    await user.selectOptions(screen.getByLabelText('System'), '200')
    await waitFor(() => {
      expect(lastSearch().get('system')).toBe('200')
      expect(lastSearch().get('talkgroup')).toBeNull()
    })
  })

  it('drops the talkgroup filter when it is set back to "any"', async () => {
    const user = userEvent.setup()
    renderApp('/search')

    await filtersLoaded()

    await user.selectOptions(screen.getByLabelText('Talkgroup'), '100:2')
    await waitFor(() => expect(lastSearch().get('talkgroup')).toBe('2'))

    await user.selectOptions(screen.getByLabelText('Talkgroup'), '')
    await waitFor(() => expect(lastSearch().get('talkgroup')).toBeNull())
    // The System stays pinned — only the Talkgroup was cleared.
    expect(lastSearch().get('system')).toBe('100')
  })

  /** Enter in a filter field must not reload the page: the filters are live,
   *  there is nothing to submit. */
  it('never submits the filter form', async () => {
    renderApp('/search')
    await resultRows()

    expect(fireEvent.submit(screen.getByRole('search'))).toBe(false)
  })

  it('converts the date inputs from local time to unix milliseconds', async () => {
    const user = userEvent.setup()
    renderApp('/search')

    await user.type(await screen.findByLabelText('From'), '2026-07-25T14:00')
    await waitFor(() =>
      expect(lastSearch().get('after')).toBe(
        String(Date.parse('2026-07-25T14:00')),
      ),
    )

    await user.type(screen.getByLabelText('To'), '2026-07-25T15:30')
    await waitFor(() =>
      expect(lastSearch().get('before')).toBe(
        String(Date.parse('2026-07-25T15:30')),
      ),
    )
  })

  it('pages through results and reports where it is', async () => {
    const user = userEvent.setup()
    // An archive one Call past a full page, so there are exactly two pages.
    const many = Array.from({ length: 51 }, (_, index) => ({
      ...ARCHIVE[0],
      id: 1000 + index,
      audioUrl: `/api/call/${1000 + index}/audio`,
    }))
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) => {
        const url = new URL(request.url)
        searches.push(url.search)
        const limit = Number(url.searchParams.get('limit'))
        const offset = Number(url.searchParams.get('offset'))
        const results = many.slice(offset, offset + limit)
        return HttpResponse.json({
          results,
          count: many.length,
          limit,
          offset,
          hasMore: offset + results.length < many.length,
        })
      }),
    )
    renderApp('/search')

    expect(await screen.findByText('1–50 of 51')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Previous page' })).toBeDisabled()

    await user.click(screen.getByRole('button', { name: 'Next page' }))

    expect(await screen.findByText('51–51 of 51')).toBeInTheDocument()
    expect(lastSearch().get('offset')).toBe('50')
    expect(await resultRows()).toHaveLength(1)
    expect(screen.getByRole('button', { name: 'Next page' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Previous page' })).toBeEnabled()

    // Going back is served from the RTK Query cache — no second round trip.
    await user.click(screen.getByRole('button', { name: 'Previous page' }))
    expect(await screen.findByText('1–50 of 51')).toBeInTheDocument()
    expect(searches.filter((search) => search.includes('offset=0'))).toHaveLength(1)
  })

  it('resets to the first page when a filter changes', async () => {
    const user = userEvent.setup()
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) => {
        const url = new URL(request.url)
        searches.push(url.search)
        return HttpResponse.json({
          ...archivePage(url),
          hasMore: url.searchParams.get('offset') === '0',
        })
      }),
    )
    renderApp('/search')

    await filtersLoaded()

    await user.click(screen.getByRole('button', { name: 'Next page' }))
    await waitFor(() => expect(lastSearch().get('offset')).toBe('50'))

    await user.selectOptions(screen.getByLabelText('Tag'), 'Fire')
    await waitFor(() => expect(lastSearch().get('offset')).toBe('0'))
  })

  describe('playback mode', () => {
    it('plays the filtered results in sequence and advances when one ends', async () => {
      const user = userEvent.setup()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[0]))

      const nowPlaying = screen.getByRole('region', { name: 'Now playing' })
      expect(within(nowPlaying).getByText('1 of 3')).toBeInTheDocument()
      const player = screen.getByTestId('call-player')
      expect(player).toHaveAttribute('src', '/api/call/3/audio')

      // The audio element ending is what makes playback *sequential*.
      player.dispatchEvent(new Event('ended'))

      await waitFor(() =>
        expect(screen.getByTestId('call-player')).toHaveAttribute(
          'src',
          '/api/call/2/audio',
        ),
      )
      await runPosition('2 of 3')
    })

    /** Spec US 25: "plays sequentially through filtered archive results **with
     *  pagination**" — reaching the end of a page rolls onto the next one and
     *  keeps playing, rather than stopping every PAGE_SIZE calls. */
    it('rolls onto the next page and keeps playing', async () => {
      const user = userEvent.setup()
      const many = Array.from({ length: 51 }, (_, index) => ({
        ...ARCHIVE[0],
        id: 1000 + index,
        audioUrl: `/api/call/${1000 + index}/audio`,
      }))
      server.use(
        http.get(`${ORIGIN}/api/calls`, ({ request }) => {
          const url = new URL(request.url)
          searches.push(url.search)
          const limit = Number(url.searchParams.get('limit'))
          const offset = Number(url.searchParams.get('offset'))
          const results = many.slice(offset, offset + limit)
          return HttpResponse.json({
            results,
            count: many.length,
            limit,
            offset,
            hasMore: offset + results.length < many.length,
          })
        }),
      )
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      // Start on the last Call of page one.
      await user.click(playIn(rows[49]))
      await runPosition('50 of 51')

      screen.getByTestId('call-player').dispatchEvent(new Event('ended'))

      // Page two loaded, and playback carried straight on into it.
      await runPosition('51 of 51')
      expect(screen.getByTestId('call-player')).toHaveAttribute(
        'src',
        '/api/call/1050/audio',
      )
      expect(lastSearch().get('offset')).toBe('50')

      // The last Call of the last page ends playback rather than looping.
      screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      await waitFor(() =>
        expect(
          screen.queryByRole('region', { name: 'Now playing' }),
        ).not.toBeInTheDocument(),
      )
      expect(lastSearch().get('offset')).toBe('50')
    })

    /**
     * The page boundary is the one transition that costs a search *and* a cold
     * audio fetch, so it is the one a listener notices — and #14's prefetch
     * stopped at the edge of the loaded page, which is exactly where it was
     * needed. Warm across it (#32): with two Calls left, fetch the next page and
     * warm its first Call's audio, so neither is being waited on when playback
     * arrives.
     */
    it('fetches the next page and warms its first Call before reaching it', async () => {
      const user = userEvent.setup()
      const { rows: many } = pagedArchive()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()

      // Two Calls left on the page (48 and 49 of 50) — the point the page-ahead
      // is armed. Nothing has been asked for beyond page one until now.
      expect(searches.filter((search) => search.includes('offset=50'))).toEqual(
        [],
      )
      await user.click(playIn(rows[47]))

      // The next page is requested before playback needs it...
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=50')),
        ).toHaveLength(1),
      )
      // ...and its first Call's audio is warmed, so arriving there is a cache
      // hit rather than a download.
      await waitFor(() =>
        expect(audioRequests).toContain(`/api/call/${many[50].id}/audio`),
      )
    })

    /** A page-ahead is decided from playback's index, and that index counts into
     *  the result set playback was *started* from. Once a filter changes, it
     *  says nothing about the results now loaded — so the page-ahead has to
     *  stand down rather than fetch page two of a set whose page one the
     *  listener has not even seen yet. */
    it('stands the page-ahead down when the filters change', async () => {
      const user = userEvent.setup()
      pagedArchive()
      renderApp('/search')
      await filtersLoaded()

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[47]))
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=50')),
        ).toHaveLength(1),
      )

      await user.selectOptions(screen.getByLabelText('Tag'), 'Fire')

      // Back to page one of the new filters...
      await waitFor(() => expect(lastSearch().get('tag')).toBe('Fire'))
      expect(lastSearch().get('offset')).toBe('0')
      // ...and no page-ahead was issued for them. The one page-ahead in the log
      // is still the pre-filter one, whose subscription and audio prefetch were
      // both dropped when its cache key stopped being current.
      expect(
        searches.filter((search) => search.includes('offset=50')),
      ).toHaveLength(1)
      expect(
        searches.filter(
          (search) => search.includes('offset=50') && search.includes('tag='),
        ),
      ).toEqual([])
    })

    /** Standing down is not enough on its own: the audio warm already on the
     *  wire has to be *dropped*, or the listener pays for bytes belonging to a
     *  result set that no longer exists.
     *
     *  This is the case RTK Query makes easy to get wrong — it **keeps its
     *  `data` when `skip` flips to true**, so the next page's results hold their
     *  identity and an effect depending only on them never re-runs, never
     *  cleans up, and never aborts. */
    it('aborts the audio warm already in flight when the filters change', async () => {
      const user = userEvent.setup()
      const { rows: many } = pagedArchive()
      const aborted: string[] = []
      server.use(
        http.get(`${ORIGIN}/api/call/:id/audio`, async ({ request }) => {
          const path = new URL(request.url).pathname
          audioRequests.push(path)
          request.signal.addEventListener('abort', () => aborted.push(path))
          // Long enough that the filter change below lands mid-flight.
          await delay(3000)
          return new HttpResponse('audio-bytes', {
            headers: { 'content-type': 'audio/mpeg' },
          })
        }),
      )
      renderApp('/search')
      await filtersLoaded()

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[47]))

      const warmed = `/api/call/${many[50].id}/audio`
      await waitFor(() => expect(audioRequests).toContain(warmed))

      await user.selectOptions(screen.getByLabelText('Tag'), 'Fire')

      await waitFor(() => expect(aborted).toContain(warmed))
    })

    /** The payoff, end to end: crossing the boundary must not re-search. The
     *  page-ahead subscribes with the *same* cache key the roll-on will use, so
     *  when playback runs off the end RTK Query already holds page two and US
     *  25's resume is immediate rather than a round trip. If the two keys ever
     *  drifted apart, the page-ahead would be pure waste and this would catch
     *  it by counting the searches. */
    it('crosses the boundary without searching for the page again', async () => {
      const user = userEvent.setup()
      pagedArchive()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[47]))
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=50')),
        ).toHaveLength(1),
      )

      // Walk off the end of page one: 48, 49, then over the edge.
      for (let step = 0; step < 3; step += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }

      await runPosition('51 of 51')
      expect(
        searches.filter((search) => search.includes('offset=50')),
      ).toHaveLength(1)
    })

    /** Every boundary, not just the first. Rolling onto a page has to re-arm the
     *  page-ahead against the page now playing, or a long run is warm across one
     *  boundary and cold across every one after it — and the second is the point
     *  at which a listener would conclude the feature does not work. */
    it('keeps warming each boundary across a long run', async () => {
      const user = userEvent.setup()
      const { rows: many } = pagedArchive(101) // three pages
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[47]))
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=50')),
        ).toHaveLength(1),
      )

      // Over the first boundary: 48, 49, then off the end onto page two.
      for (let step = 0; step < 3; step += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }
      await runPosition('51 of 101')

      // Walk page two down to its last two Calls.
      for (let step = 0; step < 47; step += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }
      await runPosition('98 of 101')

      // Page three was fetched, and its first Call warmed, exactly as page two
      // was — which only happens if rolling on re-armed the page-ahead.
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=100')),
        ).toHaveLength(1),
      )
      await waitFor(() =>
        expect(audioRequests).toContain(`/api/call/${many[100].id}/audio`),
      )
    })

    /**
     * The window on screen and the Run's own window are two different things
     * (#89), and this is what naming them apart buys: a Listener who browses
     * ahead while a Run plays moves only the screen. The Run keeps walking page
     * one and keeps warming *its* boundary — where before, the two were one
     * number, so glancing at page two stood the page-ahead down and the run
     * playing lost the warm it was about to need.
     */
    it('keeps the Run on its own page while the listener browses ahead', async () => {
      const user = userEvent.setup()
      const { rows: many } = pagedArchive(120)
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[47]))
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=50')),
        ).toHaveLength(1),
      )

      await user.click(screen.getByRole('button', { name: 'Next page' }))
      await waitFor(() => expect(lastSearch().get('offset')).toBe('50'))

      // Page two is on screen, and page three was never asked for: what the Run
      // is walking towards is page two, wherever the Listener happens to be
      // looking.
      expect(searches.filter((search) => search.includes('offset=100'))).toEqual(
        [],
      )

      // ...and it still crosses onto page two when it gets there: the Run was
      // on the 48th of 50, so three Calls end before the boundary.
      for (let ended = 0; ended < 3; ended += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }

      await waitFor(() =>
        expect(screen.getByTestId('call-player')).toHaveAttribute(
          'src',
          `/api/call/${many[50].id}/audio`,
        ),
      )
    })

    /**
     * The Listener changes the search while a Run is playing, then starts a new
     * Run on the results that arrive — and the new Run crosses its first
     * boundary before the new search's page two has landed.
     *
     * What must never happen is that it carries on with a Call from the search
     * that ended, and there is a real mechanism that would do it: an HTTP cache
     * keyed by request hands back the answer to the *previous* argument while
     * the next is in flight, so the page-ahead of the old search is still on
     * hand at exactly the moment the new Run asks for one. Both halves of the
     * fix are load-bearing — the screen reads the page for the argument it is
     * actually asking for, and the Run refuses a page whose request is not the
     * one it named.
     *
     * CONTEXT.md: "a search that changes ends it rather than silently walking
     * the wrong results."
     */
    it('never carries a Run on with a page belonging to the search that ended', async () => {
      const user = userEvent.setup()
      const before = Array.from({ length: 120 }, (_, at) => ({
        ...ARCHIVE[0],
        id: 1000 + at,
        audioUrl: `/api/call/${1000 + at}/audio`,
      }))
      const after = before.map((call, at) => ({
        ...call,
        id: 2000 + at,
        audioUrl: `/api/call/${2000 + at}/audio`,
        talkgroupTag: 'Fire',
      }))
      server.use(
        http.get(`${ORIGIN}/api/calls`, async ({ request }) => {
          const url = new URL(request.url)
          searches.push(url.search)
          const filtered = url.searchParams.get('tag') === 'Fire'
          // The new search's *second* page is slow, so the boundary is crossed
          // while the only page in hand is the old search's.
          if (filtered && url.searchParams.get('offset') === '50') {
            await delay(3000)
          }
          return HttpResponse.json(archivePage(url, filtered ? after : before))
        }),
      )
      renderApp('/search')
      await filtersLoaded()

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const first = await resultRows()
      await user.click(playIn(first[47]))
      // The old search's page two is now in hand — this is the page that must
      // not be taken.
      await waitFor(() => expect(audioRequests).toContain('/api/call/1050/audio'))

      await user.selectOptions(screen.getByLabelText('Tag'), 'Fire')
      // The new search's rows, really on screen — every row here reads
      // identically, so the download link's Call id is what tells them apart.
      await waitFor(async () =>
        expect(within((await resultRows())[0]).getByRole('link')).toHaveAttribute(
          'href',
          '/api/call/2000/download',
        ),
      )
      const second = await resultRows()
      await user.click(playIn(second[47]))
      await waitFor(() =>
        expect(screen.getByTestId('call-player')).toHaveAttribute(
          'src',
          '/api/call/2047/audio',
        ),
      )

      // Walk it off the end of the loaded page while page two is still coming.
      for (let ended = 0; ended < 3; ended += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }

      await waitFor(() =>
        expect(
          screen.queryByRole('region', { name: 'Now playing' }),
        ).not.toBeInTheDocument(),
      )
      expect(screen.getByTestId('call-player')).not.toHaveAttribute(
        'src',
        '/api/call/1050/audio',
      )
    })

    /**
     * ...and the list does *not* follow it there. The window is the Listener's,
     * moved by the paging buttons and by nothing else — a run crossing a
     * boundary every few minutes must not keep yanking the page out from under
     * someone reading it. Where playback has got to is the "Now playing"
     * readout's job, and it counts against the whole archive.
     */
    it('leaves the visible page where the listener left it as a Run rolls on', async () => {
      const user = userEvent.setup()
      pagedArchive(120)
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[49]))
      await runPosition('50 of 120')

      screen.getByTestId('call-player').dispatchEvent(new Event('ended'))

      // The Run is on page two...
      await runPosition('51 of 120')
      // ...and the list is still showing page one.
      expect(screen.getByText('1–50 of 120')).toBeInTheDocument()
    })

    /** No next page, nothing to fetch. The last page must not ask for a
     *  fifty-first result that does not exist. */
    it('asks for nothing beyond the last page', async () => {
      const user = userEvent.setup()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      // The default archive is three Calls on one page, so playing the first
      // already leaves fewer than two behind it.
      await user.click(playIn(rows[0]))

      await runPosition('1 of 3')
      expect(searches.filter((search) => search.includes('offset=3'))).toEqual(
        [],
      )
      expect(searches.filter((search) => search.includes('offset=50'))).toEqual(
        [],
      )
    })

    /** Live feed on means the archive Call *interrupts* — one Call, no run to
     *  page through, so a page-ahead would be a request for nothing. */
    it('never pages ahead while the live feed owns the audio', async () => {
      const user = userEvent.setup()
      pagedArchive()
      renderApp('/search')

      const rows = await resultRows()
      // No Playback-mode click: the live feed still owns the audio.
      await user.click(playIn(rows[47]))

      await waitFor(() =>
        expect(screen.getByTestId('call-player')).toHaveAttribute(
          'src',
          expect.stringContaining('/api/call/'),
        ),
      )
      expect(searches.filter((search) => search.includes('offset=50'))).toEqual(
        [],
      )
    })

    it('steps back and forth through the result set', async () => {
      const user = userEvent.setup()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[1]))
      await runPosition('2 of 3')

      await user.click(screen.getByRole('button', { name: 'Previous call' }))
      await runPosition('1 of 3')
      expect(screen.getByTestId('call-player')).toHaveAttribute(
        'src',
        '/api/call/3/audio',
      )

      await user.click(screen.getByRole('button', { name: 'Next call' }))
      await runPosition('2 of 3')
    })

    /** #14: pause is store state the `<audio>` element follows, so the lock
     *  screen and this button are the same control seen twice (spec US 15). */
    it('pauses the current call without losing it, then picks it up again', async () => {
      const user = userEvent.setup()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[0]))

      // The Search screen's own bar, not the shell's mini-player (#56) — both
      // pause the same transport, and this test is about this screen's.
      const bar = () =>
        within(screen.getByRole('region', { name: 'Now playing' }))
      await user.click(bar().getByRole('button', { name: 'Pause' }))
      // Still the current Call, still on the same source — paused, not dropped.
      await runPosition('1 of 3')
      expect(screen.getByTestId('call-player')).toHaveAttribute(
        'src',
        '/api/call/3/audio',
      )

      await user.click(bar().getByRole('button', { name: 'Resume' }))
      expect(bar().getByRole('button', { name: 'Pause' })).toBeInTheDocument()
    })

    it('stops playing on demand', async () => {
      const user = userEvent.setup()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(playIn(rows[0]))
      await user.click(screen.getByRole('button', { name: 'Stop' }))

      expect(
        screen.queryByRole('region', { name: 'Now playing' }),
      ).not.toBeInTheDocument()
      expect(screen.getByTestId('call-player')).not.toHaveAttribute('src')
    })
  })

  /** Spec US 26: playing an archived Call with the live feed on interrupts it,
   *  then hands straight back — the live listening queue is never touched. */
  describe('playing an archived call while the live feed is on', () => {
    it('announces the interruption and returns to the live feed when it ends', async () => {
      const user = userEvent.setup()
      renderApp('/search')

      expect(
        await screen.findByRole('button', { name: 'Playback mode' }),
      ).toHaveAttribute('aria-pressed', 'false')

      const rows = await resultRows()
      await user.click(playIn(rows[1]))

      const nowPlaying = screen.getByRole('region', { name: 'Now playing' })
      expect(
        within(nowPlaying).getByText('Interrupting live feed'),
      ).toBeInTheDocument()
      const player = screen.getByTestId('call-player')
      expect(player).toHaveAttribute('src', '/api/call/2/audio')

      // Nothing precedes an interruption, and "next" means back to the live
      // feed — the controls say so.
      expect(screen.getByRole('button', { name: 'Previous call' })).toBeDisabled()
      expect(
        screen.getByRole('button', { name: 'Back to live feed' }),
      ).toBeEnabled()

      player.dispatchEvent(new Event('ended'))

      await waitFor(() =>
        expect(
          screen.queryByRole('region', { name: 'Now playing' }),
        ).not.toBeInTheDocument(),
      )
      expect(screen.getByTestId('call-player')).not.toHaveAttribute('src')
    })
  })

  /** An uncurated archive has Systems and Talkgroups with no labels yet; the
   *  Ref is then the only name there is, and it must show everywhere. */
  it('falls back to refs when nothing has been labelled', async () => {
    const user = userEvent.setup()
    const bare = {
      id: 7,
      systemRef: 11,
      talkgroupRef: 54241,
      timestamp: Date.parse('2026-07-25T09:00:00'),
      audioUrl: '/api/call/7/audio',
    }
    server.use(
      http.get(`${ORIGIN}/api/calls`, () =>
        HttpResponse.json({
          results: [bare],
          count: 1,
          limit: 50,
          offset: 0,
          hasMore: false,
        }),
      ),
      http.get(`${ORIGIN}/api/calls/filters`, () =>
        HttpResponse.json({
          systems: [{ ref: 11 }],
          talkgroups: [{ systemRef: 11, ref: 54241 }],
          groups: [],
          tags: [],
        }),
      ),
    )
    renderApp('/search')

    const [row] = await resultRows()
    // Named the same way the scanner display names it (#11's lib/call), so a
    // Talkgroup reads identically wherever it appears.
    expect(within(row).getByText('Talkgroup 54241')).toBeInTheDocument()
    expect(within(row).getByText('System 11')).toBeInTheDocument()
    expect(
      within(screen.getByLabelText('System')).getByRole('option', {
        name: '11',
      }),
    ).toBeInTheDocument()
    expect(
      within(screen.getByLabelText('Talkgroup')).getByRole('option', {
        name: '54241',
      }),
    ).toBeInTheDocument()
    // With no calls at all there is no span to show.
    expect(screen.queryByText(/Archive spans/)).not.toBeInTheDocument()

    await user.click(playIn(row))
    expect(
      within(screen.getByRole('region', { name: 'Now playing' })).getByText(
        'Talkgroup 54241',
      ),
    ).toBeInTheDocument()
  })

  it('says so when nothing matches', async () => {
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
    renderApp('/search')

    expect(
      await screen.findByText('No calls match these filters.'),
    ).toBeInTheDocument()
    expect(screen.getByText('No calls')).toBeInTheDocument()
  })

  it('surfaces a failed search instead of showing an empty archive', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, () =>
        HttpResponse.json({ error: 'boom' }, { status: 500 }),
      ),
    )
    renderApp('/search')

    expect(await screen.findByRole('alert')).toHaveTextContent(/search failed/i)
  })

  it('has no accessibility violations', async () => {
    const { container } = renderApp('/search')
    await resultRows()

    expect(await axe(container)).toHaveNoViolations()
  })
})

describe('SearchScreen — what the recorder knew (#42)', () => {
  it('shows how long each call was, and a dash when nobody measured it', async () => {
    renderApp('/search')
    const rows = await resultRows()

    // A kerchunk and a dispatch have to be distinguishable at a glance —
    // that is the whole point of carrying the duration (spec US 8).
    expect(within(rows[0]).getByText('8.3s')).toBeInTheDocument()
    expect(within(rows[1]).getByText('1:34')).toBeInTheDocument()
    // Call 1 predates the duration column; an unknown length is a dash, never
    // a zero that would read as a kerchunk.
    expect(within(rows[2]).getByText('—')).toBeInTheDocument()
  })

  it('badges the call the emergency button was pressed on', async () => {
    renderApp('/search')
    const rows = await resultRows()

    expect(within(rows[1]).getByTitle('Emergency')).toBeInTheDocument()
    expect(within(rows[0]).queryByTitle('Emergency')).toBeNull()
  })

  /** A **Tone profile** match is a Mark like the emergency bit (#55, spec
   *  US 20), so it is badged the same way and in the same place — a Listener
   *  scanning a page is asking one question of both. */
  it('badges the call a station was paged on', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) =>
        HttpResponse.json(
          archivePage(new URL(request.url), [
            {
              ...ARCHIVE[0],
              id: 91,
              tone: true,
              tones: [{ label: 'Station 12', atMs: 1840 }],
            },
            { ...ARCHIVE[0], id: 92 },
          ]),
        ),
      ),
    )
    renderApp('/search')
    const rows = await resultRows()

    // **The station, not just the fact.** An operator with twelve stations on
    // one dispatch channel is asking who was paged, and the badge's label is
    // the only thing a listener — or a screen reader — actually reads.
    expect(
      within(rows[0]).getByTitle('Tone-out: Station 12'),
    ).toBeInTheDocument()
    expect(within(rows[1]).queryByTitle(/^Tone-out/)).toBeNull()
  })

  /** A Call marked by a profile whose pages could not be read back still says a
   *  page happened, which is the true half of what is known. */
  it('badges a page it cannot name', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) =>
        HttpResponse.json(
          archivePage(new URL(request.url), [
            { ...ARCHIVE[0], id: 93, tone: true },
          ]),
        ),
      ),
    )
    renderApp('/search')
    const rows = await resultRows()

    expect(within(rows[0]).getByTitle('Tone-out')).toBeInTheDocument()
  })

  /** ...and it is **filterable**, over the same closed vocabulary a Webhook
   *  fires on — one control rather than a checkbox per mark, so a mark added
   *  later is an entry in `MARKS` and nothing else. */
  it('filters the archive by mark', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    await resultRows()

    await user.selectOptions(screen.getByLabelText('Mark'), 'Tone-out')
    await waitFor(() => expect(lastSearch().get('mark')).toBe('tone'))

    // Every mark in the vocabulary, over one control — and clearing it goes
    // back to the unfiltered search, which is asserted on the *select* rather
    // than on a request because that page is already in the cache and a cached
    // page issues none (the `run.ts` note).
    await user.selectOptions(screen.getByLabelText('Mark'), 'Emergency')
    await waitFor(() => expect(lastSearch().get('mark')).toBe('emergency'))

    await user.selectOptions(screen.getByLabelText('Mark'), 'Any call')
    expect(screen.getByLabelText('Mark')).toHaveValue('')
  })

  it('badges an encrypted call and offers no way to play or download it', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) =>
        HttpResponse.json(
          archivePage(new URL(request.url), [
            {
              id: 9,
              systemRef: 100,
              systemLabel: 'Alpha',
              talkgroupRef: 5,
              talkgroupLabel: 'Sheriff Tac 1',
              timestamp: Date.parse('2026-07-25T15:00:00'),
              durationMs: 4000,
              encrypted: true,
            },
          ]),
        ),
      ),
    )
    renderApp('/search')
    const rows = await resultRows()

    expect(within(rows[0]).getByTitle('Encrypted')).toBeInTheDocument()
    // There is nothing behind those controls: the server sends no audioUrl for
    // an encrypted Call, so offering a play button would offer a 404.
    expect(within(rows[0]).queryByRole('button', { name: /^Play/ })).toBeNull()
    expect(within(rows[0]).queryByRole('link', { name: /^Download/ })).toBeNull()
  })

  /**
   * **Patch provenance** (#56, spec US 54). A **Patch** is a console-made union
   * of Talkgroups (CONTEXT.md), and a transmission that arrived on one is a
   * different fact about the same row: the channel it is filed under is not the
   * only channel it was on.
   *
   * The chip says which — an Operator reading a page wants to know a Call on
   * FD Dispatch also went out on TAC 3, not merely that *something* was
   * patched. rdio-scanner parses `patches[]`, routes on it, and then shows a
   * Listener nothing at all.
   */
  it('badges a call that arrived on a patch, naming the channels', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) =>
        HttpResponse.json(
          archivePage(new URL(request.url), [
            { ...ARCHIVE[0], id: 95, patches: [54242, 54255] },
            { ...ARCHIVE[0], id: 96 },
          ]),
        ),
      ),
    )
    renderApp('/search')
    const rows = await resultRows()

    expect(
      within(rows[0]).getByTitle('Patched to 54242, 54255'),
    ).toBeInTheDocument()
    expect(within(rows[1]).queryByTitle(/^Patched/)).toBeNull()
  })

  /** An empty array is not a patch. The server omits the key when there is
   *  none, but a Call that has been through a client that serializes an empty
   *  one must not read as patched to nothing. */
  it('does not badge a call whose patch list is empty', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) =>
        HttpResponse.json(
          archivePage(new URL(request.url), [
            { ...ARCHIVE[0], id: 97, patches: [] },
          ]),
        ),
      ),
    )
    renderApp('/search')
    const rows = await resultRows()

    expect(within(rows[0]).queryByTitle(/^Patched/)).toBeNull()
  })

  /** Searching by radio (#47, spec US 44). A typed Ref rather than a dropdown:
   *  a county has tens of thousands of radios, and offering them as options
   *  would put an unbounded list in every filter response. */
  it('searches by the radio that was heard', async () => {
    renderApp('/search')
    await filtersLoaded()

    await userEvent.type(screen.getByLabelText('Unit'), '1200')

    await waitFor(() => expect(lastSearch().get('unit')).toBe('1200'))
  })

  /** Emptying the box means "any radio", never radio **zero** — which would
   *  search for a Ref no recorder sends and quietly return nothing. Asserted
   *  over every request the screen made, because clearing back to the search it
   *  started on is answered from cache and sends none. */
  it('clears the unit filter rather than searching for radio zero', async () => {
    renderApp('/search')
    await filtersLoaded()

    const unit = screen.getByLabelText('Unit')
    await userEvent.type(unit, '1200')
    await waitFor(() => expect(lastSearch().get('unit')).toBe('1200'))

    await userEvent.clear(unit)

    expect(unit).toHaveValue(null)
    expect(
      searches.filter((query) => new URLSearchParams(query).get('unit') === '0'),
    ).toEqual([])
  })

  /** Every row says who keyed it, **in a column of its own** beside the length
   *  (spec US 36: "duration and unit columns"), and every one of those is
   *  tappable through to that radio's history — which is the whole of
   *  "reachable from any rendered unit label" on the screen a listener spends
   *  the most time on. */
  it('names the radio on the rows that have one, and leaves the rest alone', async () => {
    renderApp('/search')
    const rows = await resultRows()

    // `ARCHIVE`'s oldest Call is the one keyed by a named radio.
    expect(
      within(rows[2]).getByRole('link', { name: 'History for unit Engine 1' }),
    ).toHaveAttribute('href', '/unit/100/1200')
    expect(within(rows[0]).queryByRole('link', { name: /History for unit/ })).toBeNull()
  })

  it('filters out the kerchunks, in whole seconds', async () => {
    renderApp('/search')
    await filtersLoaded()

    await userEvent.selectOptions(
      screen.getByLabelText('Min duration'),
      '5',
    )

    await waitFor(() => expect(lastSearch().get('minDuration')).toBe('5'))
  })

  it('brings the unmeasured Calls back when the filter is cleared', async () => {
    // A Call with no `durationMs` matches no threshold — so "any" has to mean
    // *absent*, not `minDuration=0`. This is what that costs if it is wrong:
    // the pre-#42 half of an operator's archive would be unreachable from the
    // moment they touched the control.
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) => {
        const url = new URL(request.url)
        searches.push(url.search)
        const floor = url.searchParams.get('minDuration')
        const rows =
          floor === null
            ? ARCHIVE
            : ARCHIVE.filter(
                (call) => (call.durationMs ?? -1) >= Number(floor) * 1000,
              )
        return HttpResponse.json(archivePage(url, rows))
      }),
    )
    renderApp('/search')
    await filtersLoaded()
    expect(await resultRows()).toHaveLength(3)

    const select = screen.getByLabelText('Min duration')
    await userEvent.selectOptions(select, '5')
    await waitFor(async () => expect(await resultRows()).toHaveLength(2))

    await userEvent.selectOptions(select, '')
    await waitFor(async () => expect(await resultRows()).toHaveLength(3))
  })
})

describe('SearchScreen — multi-site coverage (#42, spec US 11)', () => {
  it('names the tower a Call was heard on, and stays quiet when there is none', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) =>
        HttpResponse.json(
          archivePage(new URL(request.url), [
            { ...ARCHIVE[0], id: 20, siteRef: 3 },
            { ...ARCHIVE[1], id: 21 },
          ]),
        ),
      ),
    )
    renderApp('/search')
    const rows = await resultRows()

    expect(within(rows[0]).getByText(/Site 3/)).toBeInTheDocument()
    expect(within(rows[1]).queryByText(/Site/)).toBeNull()
  })
})

/**
 * The Archive as a place you can link to (#61, spec US 30–31).
 *
 * The URL *is* the search state, not a copy of it kept in step with one — which
 * is what makes a reload, a tab switch and the back button one mechanism rather
 * than three features that each have to remember the others.
 */
describe('SearchScreen — URL-addressable search (#61, spec US 30)', () => {
  it('restores every filter a link carries', async () => {
    renderApp('/search?tag=Fire&system=100&sort=oldest')
    await filtersLoaded()

    expect(screen.getByLabelText('Tag')).toHaveValue('Fire')
    expect(screen.getByLabelText('System')).toHaveValue('100')
    expect(screen.getByLabelText('Sort')).toHaveValue('oldest')
    expect(lastSearch().get('tag')).toBe('Fire')
    expect(lastSearch().get('sort')).toBe('oldest')
  })

  it('writes a filter change into the URL, so the view is the address', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    await filtersLoaded()

    await user.selectOptions(screen.getByLabelText('Tag'), 'Fire')

    await waitFor(() => expect(routerProbe.location).toBe('/search?tag=Fire'))
  })

  it('carries the page, so a link lands on the page that was being read', async () => {
    const user = userEvent.setup()
    pagedArchive()
    renderApp('/search')
    await resultRows()

    await user.click(screen.getByRole('button', { name: 'Next page' }))

    await waitFor(() => expect(routerProbe.location).toBe('/search?offset=50'))
  })

  /** A dropdown is one decision and earns one history entry; a number typed
   *  into a box is not four. Back has to return to the search *before* the
   *  typing, not walk it digit by digit. */
  it('does not put a history entry behind every keystroke', async () => {
    const user = userEvent.setup()
    renderApp('/search?tag=Fire')
    await filtersLoaded()

    await user.type(screen.getByLabelText('Unit'), '1234')
    await waitFor(() => expect(routerProbe.location).toBe('/search?tag=Fire&unit=1234'))

    act(() => routerProbe.go(-1))

    await waitFor(() => expect(routerProbe.location).toBe('/search?tag=Fire'))
  })

  it('goes back to the search before it', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    await filtersLoaded()
    await user.selectOptions(screen.getByLabelText('Tag'), 'Fire')
    await waitFor(() => expect(routerProbe.location).toBe('/search?tag=Fire'))

    act(() => routerProbe.go(-1))

    // The search that was left is the search that comes back — filters and all.
    // Deliberately not asserted against the *network*: the unfiltered page is
    // already cached, and going back to it costing no request is the point of
    // caching rather than a failure to restore anything.
    await waitFor(() => expect(routerProbe.location).toBe('/search'))
    expect(screen.getByLabelText('Tag')).toHaveValue('')
  })
})

describe('SearchScreen — date presets and reset (#61, spec US 31)', () => {
  /** Presets resolve to *instants* (`lib/dateRange`), so what the URL carries
   *  and what the inputs show is the range actually being searched — which is
   *  what makes the link a view somebody else can open. */
  it('fills both date inputs from one tap, and says so in the URL', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    await filtersLoaded()

    await user.click(screen.getByRole('button', { name: 'Today' }))

    const midnight = new Date()
    midnight.setHours(0, 0, 0, 0)
    await waitFor(() =>
      expect(lastSearch().get('after')).toBe(String(midnight.getTime())),
    )
    expect(screen.getByLabelText('From')).toHaveValue(
      msToDateTimeLocal(midnight.getTime()),
    )
    expect(screen.getByLabelText('To')).not.toHaveValue('')
  })

  it('shows the bounds a link carries, rather than an empty box over a filtered list', async () => {
    const after = Date.parse('2026-07-25T14:00')
    renderApp(`/search?after=${after}`)
    await filtersLoaded()

    expect(screen.getByLabelText('From')).toHaveValue('2026-07-25T14:00')
  })

  it('clears everything at once, which is the eight taps the reset exists to save', async () => {
    const user = userEvent.setup()
    renderApp('/search?tag=Fire&system=100&after=1000&sort=oldest&offset=50')
    await filtersLoaded()

    await user.click(screen.getByRole('button', { name: 'Reset filters' }))

    await waitFor(() => expect(routerProbe.location).toBe('/search'))
    expect(screen.getByLabelText('Tag')).toHaveValue('')
    expect(screen.getByLabelText('From')).toHaveValue('')
    expect(screen.getByLabelText('Sort')).toHaveValue('newest')
  })
})

describe('SearchScreen — a Call you can link to (#61, spec US 30)', () => {
  it('opens playing the Call a link names', async () => {
    renderApp('/search?call=2')

    // Named in the Run's own bar, and loaded into the one shared `<audio>`
    // element — the two halves of "opens playing", neither of which the other
    // proves on its own.
    const playing = await screen.findByRole('region', { name: 'Now playing' })
    expect(within(playing).getByText('Alpha Law')).toBeInTheDocument()
    await waitFor(() =>
      expect(screen.getByTestId('call-player')).toHaveAttribute(
        'src',
        '/api/call/2/audio',
      ),
    )
  })

  it('plays a Call the current filters would not have found', async () => {
    // The link is resolved by id, not searched for, so a Call outside the
    // filters on screen still plays — which is what makes it a *deep* link.
    renderApp('/search?call=2&tag=Law')
    const playing = await screen.findByRole('region', { name: 'Now playing' })

    expect(within(playing).getByText('Alpha Law')).toBeInTheDocument()
  })

  /** The link has been *acted on*, so it leaves the address bar — where the
   *  filters stay, because they describe what is on screen. Without this, the
   *  shell remembers `?call=2` as where the Search tab was and re-plays the
   *  Call every time the Listener comes back to it. */
  it('does not play it again when the listener comes back to the tab', async () => {
    const user = userEvent.setup()
    renderApp('/search?call=2')
    await screen.findByRole('region', { name: 'Now playing' })
    await waitFor(() => expect(routerProbe.location).toBe('/search'))

    await user.click(within(screen.getByRole('region', { name: 'Now playing' })).getByRole('button', { name: 'Stop' }))
    expect(screen.queryByRole('region', { name: 'Now playing' })).toBeNull()

    await user.click(screen.getByRole('link', { name: 'Live' }))
    await screen.findByRole('heading', { name: 'LIVE' })
    await user.click(screen.getByRole('link', { name: 'Search' }))
    await screen.findByRole('search', { name: 'Archive filters' })

    expect(screen.queryByRole('region', { name: 'Now playing' })).toBeNull()
  })

  it('keeps saying so while the Call a link names is gone', async () => {
    renderApp('/search?call=9999')
    await screen.findByText(/that call is no longer in the archive/i)

    // Not consumed: nothing was acted on, so the address still describes what
    // was asked for and the sentence does not vanish on the next render.
    expect(routerProbe.location).toBe('/search?call=9999')
  })

  it('says so when the Call a link names is gone', async () => {
    renderApp('/search?call=9999')

    expect(
      await screen.findByText(/that call is no longer in the archive/i),
    ).toBeInTheDocument()
  })
})

describe('SearchScreen — playing forward in time from a row (#61)', () => {
  it('runs oldest-first from the Call that was tapped, whatever the list is sorted by', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    const rows = await resultRows()

    // The list is newest-first, so the middle row's Play would walk *back*
    // through history. This is the other direction.
    await user.click(
      within(rows[1]).getByRole('button', { name: /^Play forward from/ }),
    )

    await waitFor(() => expect(lastSearch().get('sort')).toBe('oldest'))
    expect(lastSearch().get('after')).toBe(String(ARCHIVE[1].timestamp))
  })

  it('says so when the archive will not answer for the run ahead', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    const rows = await resultRows()
    server.use(
      http.get(`${ORIGIN}/api/calls`, () => new HttpResponse('nope', { status: 500 })),
    )

    await user.click(
      within(rows[1]).getByRole('button', { name: /^Play forward from/ }),
    )

    expect(await screen.findByText(/could not play forward/i)).toBeInTheDocument()
  })

  it('leaves the list on screen sorted as the listener left it', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    const rows = await resultRows()

    await user.click(
      within(rows[1]).getByRole('button', { name: /^Play forward from/ }),
    )
    await waitFor(() => expect(lastSearch().get('sort')).toBe('oldest'))

    // A **Run** carries its own window and its own search; browsing is the
    // screen's (#89). Turning round to play forward must not re-sort the list
    // the Listener is reading.
    expect(screen.getByLabelText('Sort')).toHaveValue('newest')
    expect(routerProbe.location).toBe('/search')
  })
})

describe('SearchScreen — sending a link (#61, spec US 30)', () => {
  it('hands the platform a link to this search', async () => {
    const user = userEvent.setup()
    const shared = sharesTo()
    renderApp('/search?tag=Fire')
    await filtersLoaded()

    await user.click(screen.getByRole('button', { name: 'Copy link to this search' }))

    await waitFor(() => expect(shared()).toBe('http://localhost/search?tag=Fire'))
  })

  it('hands the platform a link that opens on one Call', async () => {
    const user = userEvent.setup()
    const shared = sharesTo()
    renderApp('/search')
    const rows = await resultRows()

    await user.click(within(rows[1]).getByRole('button', { name: /^Copy link to/ }))

    await waitFor(() => expect(shared()).toBe('http://localhost/search?call=2'))
  })

  /** A notice is a thing that just happened, not a thing that is true — so it
   *  goes of its own accord rather than sitting under the filters all session. */
  it('takes its confirmation back down', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    sharesTo()
    renderApp('/search?tag=Fire')
    await filtersLoaded()
    await user.click(screen.getByRole('button', { name: 'Copy link to this search' }))
    expect(await screen.findByText('Link copied.')).toBeInTheDocument()

    act(() => void vi.advanceTimersByTime(NOTICE_MS))

    expect(screen.queryByText('Link copied.')).toBeNull()
    vi.useRealTimers()
  })

  it('says so when the browser will not let it', async () => {
    const user = userEvent.setup()
    sharesTo({ fails: true })
    renderApp('/search?tag=Fire')
    await filtersLoaded()

    await user.click(screen.getByRole('button', { name: 'Copy link to this search' }))

    expect(await screen.findByText(/could not copy/i)).toBeInTheDocument()
  })
})

describe('SearchScreen — the public share link (#64, spec US 32)', () => {
  /** Every `POST /api/call/{id}/share` this screen made, and what the server
   *  answered with. */
  function mints({ fails = false } = {}) {
    const asked: string[] = []
    server.use(
      http.post(`${ORIGIN}/api/call/:id/share`, ({ params }) => {
        asked.push(String(params.id))
        if (fails) {
          return HttpResponse.text('share link not found\n', { status: 404 })
        }
        return HttpResponse.json({
          url: `/s?t=tok${params.id}`,
          expiresAtMs: 1_700_600_000_000,
        })
      }),
    )
    return () => asked
  }

  /**
   * **The whole gesture, in one tap.** The link's spelling is the server's —
   * what the browser adds is the origin it actually reached this Instance on,
   * which is the one thing the server cannot know.
   */
  it('mints a public link and hands it to the platform', async () => {
    const user = userEvent.setup()
    const shared = sharesTo()
    const asked = mints()
    renderApp('/search')
    const rows = await resultRows()

    await user.click(within(rows[1]).getByRole('button', { name: /^Share .* publicly$/ }))

    await waitFor(() => expect(shared()).toBe('http://localhost/s?t=tok2'))
    expect(asked()).toEqual(['2'])
  })

  /** Two links, deliberately, because they are two different offers: one opens
   *  the Call in the app and never expires, the other opens it for somebody who
   *  has never heard of this instance and stops working. */
  it('is a second control beside the in-app link', async () => {
    renderApp('/search')
    const rows = await resultRows()

    expect(within(rows[1]).getByRole('button', { name: /^Copy link to/ })).toBeInTheDocument()
    expect(within(rows[1]).getByRole('button', { name: /publicly$/ })).toBeInTheDocument()
  })

  /** **A control that is offered and then refused is a control that lies.** An
   *  Instance with `[share] enabled = false` says so in its catalog, and the
   *  control is simply not there. */
  it('is not offered where the instance does not mint links', async () => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () =>
        HttpResponse.json({ systems: [], activityWindowMs: 86_400_000, sharing: false }),
      ),
    )
    renderApp('/search')
    const rows = await resultRows()

    expect(within(rows[1]).queryByRole('button', { name: /publicly$/ })).toBeNull()
  })

  /** A Call that has since been pruned, or an Operator who closed sharing while
   *  this page was open: nothing was copied, and somebody is waiting to paste. */
  it('says so when a link could not be minted', async () => {
    const user = userEvent.setup()
    sharesTo()
    mints({ fails: true })
    renderApp('/search')
    const rows = await resultRows()

    await user.click(within(rows[1]).getByRole('button', { name: /^Share .* publicly$/ }))

    expect(await screen.findByText('Could not create a share link.')).toBeInTheDocument()
  })
})

describe('SearchScreen — the density ribbon and the heatmap (#62, spec US 34–35)', () => {
  /** The ribbon, once the aggregate has landed. */
  const ribbon = () => screen.findByTestId('density-ribbon')

  /** Every `/api/calls/activity` query string the screen sent, in order. */
  let activity: string[] = []

  /**
   * An archive spread across `total` hours, newest first.
   *
   * [`pagedArchive`] stamps every row with the same instant, which is fine for
   * paging and useless here: a ribbon over one instant is a single bucket, and
   * every jump would be a jump to where the Listener already is.
   */
  function spreadArchive(total = 51) {
    const rows = Array.from({ length: total }, (_, index) => ({
      ...ARCHIVE[0],
      id: 2000 + index,
      timestamp: Date.parse('2026-07-25T14:00:00') - index * 3_600_000,
      audioUrl: `/api/call/${2000 + index}/audio`,
    }))
    server.use(
      http.get(`${ORIGIN}/api/calls`, ({ request }) => {
        const url = new URL(request.url)
        searches.push(url.search)
        return HttpResponse.json(archivePage(url, rows))
      }),
      http.get(`${ORIGIN}/api/calls/activity`, ({ request }) => {
        const url = new URL(request.url)
        activity.push(url.search)
        return HttpResponse.json(activitySeries(url, rows))
      }),
    )
    return rows
  }

  beforeEach(() => {
    activity = []
    server.use(
      http.get(`${ORIGIN}/api/calls/activity`, ({ request }) => {
        const url = new URL(request.url)
        activity.push(url.search)
        return HttpResponse.json(activitySeries(url))
      }),
    )
  })

  /**
   * **The bars describe the results above them.** That is the ribbon's one
   * claim, and it is only true because both come from the same filters — so
   * the request is asserted to carry them.
   */
  it('asks for the activity of the search that is on screen', async () => {
    renderApp('/search?tag=Fire&system=100')
    await ribbon()

    const asked = new URLSearchParams(activity.at(-1))
    expect(asked.get('tag')).toBe('Fire')
    expect(asked.get('system')).toBe('100')
    // ...and never the window, which is what keeps a page turn from refetching
    // the whole aggregate.
    expect(asked.get('offset')).toBeNull()
    expect(asked.get('limit')).toBeNull()
  })

  it('draws a bar for every bucket the server answered with', async () => {
    renderApp('/search')

    const drawn = await ribbon()
    const series = activitySeries(
      new URL(`${ORIGIN}/api/calls/activity${activity.at(-1)}`),
    )
    expect(drawn.querySelectorAll('span')).toHaveLength(series.values.length)
  })

  /** Turning a page must not re-issue the aggregate: it is a second grouped
   *  query over the whole filtered archive, and paying for one per page turn
   *  is exactly what a Pi cannot afford. */
  it('does not refetch the activity when the window moves', async () => {
    const user = userEvent.setup()
    spreadArchive()
    renderApp('/search')
    await ribbon()
    const asked = activity.length

    await user.click(await screen.findByRole('button', { name: 'Next page' }))
    await screen.findByText('51–51 of 51')

    expect(activity).toHaveLength(asked)
  })

  /**
   * **Jumping is a window move, not a filter change** — the search in the URL
   * is untouched, so a **Run** that is walking keeps walking (#89).
   */
  it('moves the window without touching the search', async () => {
    const user = userEvent.setup()
    spreadArchive()
    renderApp('/search?tag=Fire')
    const drawn = await ribbon()

    // Home is the oldest bucket; newest-first results put it at the far end.
    drawn.focus()
    await user.keyboard('{Home}')

    await waitFor(() => expect(routerProbe.location).toMatch(/offset=/))
    expect(routerProbe.location).toContain('tag=Fire')
    expect(lastSearch().get('offset')).not.toBe('0')
  })

  /** The other end of the same control: End is the newest bucket, which is
   *  page one — so a Listener can always get back. */
  it('goes back to the first page from the newest end', async () => {
    const user = userEvent.setup()
    spreadArchive()
    renderApp('/search?offset=50')
    const drawn = await ribbon()
    await screen.findByText('51–51 of 51')

    drawn.focus()
    await user.keyboard('{End}')

    await waitFor(() => expect(lastSearch().get('offset')).toBe('0'))
    expect(routerProbe.location).not.toContain('offset')
  })

  /** A ribbon a test can drag: the element has no layout under jsdom, so it is
   *  given one. */
  async function draggable() {
    const drawn = await ribbon()
    drawn.getBoundingClientRect = () =>
      ({ left: 0, width: 100, top: 0, height: 40 }) as DOMRect
    drawn.setPointerCapture = () => {}
    return drawn
  }

  /**
   * **A drag moves the marker; releasing moves the window.**
   *
   * The whole gesture, and the assertion that matters is the *middle* one: a
   * ribbon that jumped live would issue a fresh page query for every bucket the
   * thumb crossed — a slow drag across a hundred and twenty bars is a hundred
   * and twenty searches, on the Pi this is written for.
   */
  it('moves the window when a drag ends, not while it is happening', async () => {
    // Four pages deep, so a jump to the far end is a window move the URL can
    // actually show: over one page every bucket is offset zero.
    spreadArchive(200)
    renderApp('/search')
    const drawn = await draggable()
    const before = searches.length

    fireEvent.pointerDown(drawn, { clientX: 98, pointerId: 1 })
    fireEvent.pointerMove(drawn, { clientX: 50, pointerId: 1 })
    fireEvent.pointerMove(drawn, { clientX: 2, pointerId: 1 })

    expect(searches).toHaveLength(before)
    expect(routerProbe.location).not.toContain('offset')

    fireEvent.pointerUp(drawn, { clientX: 2, pointerId: 1 })

    await waitFor(() => expect(routerProbe.location).toContain('offset='))
  })

  /** ...and the readout says where the drag is going, which is the only thing
   *  on screen that makes "jump to date" a date rather than a guess. */
  it('says which moment the thumb is over while dragging', async () => {
    spreadArchive(200)
    renderApp('/search')
    const drawn = await draggable()
    const readout = () => screen.getByTestId('ribbon-at').textContent ?? ''
    const instant = (text: string) => Date.parse(text.split(' · ')[0])
    // The window starts at the newest page, so the marker starts at the newest
    // end of the ribbon.
    const newest = readout()

    fireEvent.pointerDown(drawn, { clientX: 0, pointerId: 1 })

    expect(readout()).not.toBe(newest)
    expect(instant(readout())).toBeLessThan(instant(newest))
    expect(readout()).toMatch(/calls?$/)
  })

  /** A pointer taken away — a browser claiming the gesture for a scroll — is
   *  not a decision, so nothing moves. */
  it('abandons a drag the browser cancels', async () => {
    spreadArchive(200)
    renderApp('/search')
    const drawn = await draggable()

    fireEvent.pointerDown(drawn, { clientX: 2, pointerId: 1 })
    fireEvent.pointerCancel(drawn, { clientX: 2, pointerId: 1 })

    expect(routerProbe.location).not.toContain('offset')
  })

  /** Nothing to draw is nothing drawn: an empty ribbon over "no calls match
   *  these filters" says nothing the sentence below it does not. */
  it('is absent when the search found nothing', async () => {
    server.use(
      http.get(`${ORIGIN}/api/calls`, () =>
        HttpResponse.json({ results: [], count: 0, limit: 50, offset: 0, hasMore: false }),
      ),
      http.get(`${ORIGIN}/api/calls/activity`, () =>
        HttpResponse.json({ fromMs: 0, toMs: 1000, bucketMs: 1000, values: [0] }),
      ),
    )
    renderApp('/search')
    await screen.findByText('No calls match these filters.')

    expect(screen.queryByTestId('density-ribbon')).toBeNull()
  })

  /**
   * The heatmap is the second view of the same search, and it is **not**
   * fetched until somebody looks at it — a chart nobody opened is a round trip
   * nobody asked for.
   */
  it('fetches the hourly buckets only when the heatmap is opened', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    await ribbon()
    expect(activity.some((query) => query.includes('bucketMs'))).toBe(false)

    await user.click(screen.getByRole('button', { name: 'By hour' }))

    await screen.findByRole('table')
    expect(activity.some((query) => query.includes('bucketMs=3600000'))).toBe(true)
  })

  it('shows a week of hours, in the listener`s own clock', async () => {
    const user = userEvent.setup()
    renderApp('/search')
    await ribbon()

    await user.click(screen.getByRole('button', { name: 'By hour' }))

    const grid = await screen.findByRole('table')
    expect(within(grid).getByRole('rowheader', { name: 'Mon' })).toBeInTheDocument()
    expect(within(grid).getByRole('rowheader', { name: 'Sun' })).toBeInTheDocument()
    // Seven day rows plus the hour header.
    expect(within(grid).getAllByRole('row')).toHaveLength(8)
  })

  /** A pointer moving over the ribbon with no button down is somebody's thumb
   *  passing by, not a scrub. */
  it('ignores a pointer that is not dragging', async () => {
    spreadArchive(200)
    renderApp('/search')
    const drawn = await draggable()
    const before = screen.getByTestId('ribbon-at').textContent

    fireEvent.pointerMove(drawn, { clientX: 2, pointerId: 1 })
    fireEvent.pointerUp(drawn, { clientX: 2, pointerId: 1 })

    expect(screen.getByTestId('ribbon-at')).toHaveTextContent(before ?? '')
    expect(routerProbe.location).not.toContain('offset')
  })

  /** An element with no layout — which is what a ribbon is before the browser
   *  has measured it — has no coordinates to read a bucket out of, so a press
   *  on it means nothing rather than `NaN`. */
  it('reads nothing from a ribbon the browser has not laid out', async () => {
    spreadArchive(200)
    renderApp('/search')
    const drawn = await ribbon()
    drawn.setPointerCapture = () => {}
    const before = screen.getByTestId('ribbon-at').textContent

    fireEvent.pointerDown(drawn, { clientX: 40, pointerId: 1 })
    fireEvent.pointerUp(drawn, { clientX: 40, pointerId: 1 })

    expect(screen.getByTestId('ribbon-at')).toHaveTextContent(before ?? '')
    expect(routerProbe.location).not.toContain('offset')
  })

  /** Jumping to where you already are is not a navigation, so the address bar
   *  does not gain an entry for it. */
  it('does nothing when the jump is to the bucket already on screen', async () => {
    const user = userEvent.setup()
    spreadArchive(200)
    renderApp('/search')
    const drawn = await ribbon()
    const before = searches.length

    drawn.focus()
    // The window starts at the newest page, so End is already where it is.
    await user.keyboard('{End}')

    expect(searches).toHaveLength(before)
    expect(routerProbe.location).not.toContain('offset')
  })

  /** Arrowing along it moves one bucket at a time — the ARIA slider contract,
   *  so the ribbon is usable with no pointer at all. */
  it('scrubs one bucket at a time with the arrow keys', async () => {
    const user = userEvent.setup()
    spreadArchive(200)
    renderApp('/search')
    const drawn = await ribbon()
    const at = () => Number(drawn.getAttribute('aria-valuenow'))
    drawn.focus()
    await user.keyboard('{Home}')
    await waitFor(() => expect(at()).toBe(0))

    await user.keyboard('{ArrowRight}{ArrowRight}')

    // The thumb moves a bucket a press, and stays where it was put: the window
    // is a page, so the offset behind it cannot represent a single bucket.
    expect(at()).toBe(2)
    await user.keyboard('{ArrowLeft}')
    expect(at()).toBe(1)
  })

  /** ...and putting the ribbon down puts the thumb back on the window, so one
   *  that nobody is touching always says where the Listener actually is. */
  it('returns the thumb to the window when it loses focus', async () => {
    const user = userEvent.setup()
    spreadArchive(200)
    renderApp('/search')
    const drawn = await ribbon()
    const at = () => Number(drawn.getAttribute('aria-valuenow'))
    drawn.focus()
    await user.keyboard('{Home}')
    expect(at()).toBe(0)

    fireEvent.blur(drawn)

    expect(at()).toBeGreaterThan(0)
  })

  /** Keys the ribbon does not use are left to the page — a slider that
   *  swallowed Tab would trap a keyboard listener inside it. */
  it('leaves keys it does not use alone', async () => {
    const user = userEvent.setup()
    spreadArchive(200)
    renderApp('/search')
    const drawn = await ribbon()

    drawn.focus()
    await user.keyboard('a')

    expect(routerProbe.location).not.toContain('offset')
  })

  /** A range the server had to widen is a grid this could only draw wrongly —
   *  three-hour totals filed under whichever hour they began in — so it says so
   *  instead. */
  it('refuses to draw a heatmap the server could not answer hourly', async () => {
    const user = userEvent.setup()
    server.use(
      http.get(`${ORIGIN}/api/calls/activity`, ({ request }) => {
        const url = new URL(request.url)
        const series = activitySeries(url)
        return HttpResponse.json(
          url.searchParams.has('bucketMs')
            ? { ...series, bucketMs: 3 * 3_600_000 }
            : series,
        )
      }),
    )
    renderApp('/search')
    await ribbon()

    await user.click(screen.getByRole('button', { name: 'By hour' }))

    expect(await screen.findByText(/too long to break into hours/i)).toBeInTheDocument()
    expect(screen.queryByRole('table')).toBeNull()
  })

  it('has no accessibility violations with the charts on screen', async () => {
    const user = userEvent.setup()
    const { container } = renderApp('/search')
    await ribbon()
    await user.click(screen.getByRole('button', { name: 'By hour' }))
    await screen.findByRole('table')

    expect(await axe(container)).toHaveNoViolations()
  })
})
