import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { delay, http, HttpResponse } from 'msw'
import { axe } from 'vitest-axe'
import { beforeEach, describe, expect, it } from 'vitest'

import { ARCHIVE, ORIGIN, archivePage } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'

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

/** The result rows, once the first page has landed. */
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
      await user.click(within(rows[0]).getByRole('button', { name: /^Play / }))

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
      expect(screen.getByText('2 of 3')).toBeInTheDocument()
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
      await user.click(within(rows[49]).getByRole('button', { name: /^Play / }))
      expect(screen.getByText('50 of 51')).toBeInTheDocument()

      screen.getByTestId('call-player').dispatchEvent(new Event('ended'))

      // Page two loaded, and playback carried straight on into it.
      expect(await screen.findByText('51 of 51')).toBeInTheDocument()
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
      await user.click(within(rows[47]).getByRole('button', { name: /^Play / }))

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
      await user.click(within(rows[47]).getByRole('button', { name: /^Play / }))
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
      await user.click(within(rows[47]).getByRole('button', { name: /^Play / }))

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
      await user.click(within(rows[47]).getByRole('button', { name: /^Play / }))
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=50')),
        ).toHaveLength(1),
      )

      // Walk off the end of page one: 48, 49, then over the edge.
      for (let step = 0; step < 3; step += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }

      expect(await screen.findByText('51 of 51')).toBeInTheDocument()
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
      await user.click(within(rows[47]).getByRole('button', { name: /^Play / }))
      await waitFor(() =>
        expect(
          searches.filter((search) => search.includes('offset=50')),
        ).toHaveLength(1),
      )

      // Over the first boundary: 48, 49, then off the end onto page two.
      for (let step = 0; step < 3; step += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }
      expect(await screen.findByText('51 of 101')).toBeInTheDocument()

      // Walk page two down to its last two Calls.
      for (let step = 0; step < 47; step += 1) {
        screen.getByTestId('call-player').dispatchEvent(new Event('ended'))
      }
      expect(await screen.findByText('98 of 101')).toBeInTheDocument()

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
      await user.click(within(rows[47]).getByRole('button', { name: /^Play / }))
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
      await user.click(within(first[47]).getByRole('button', { name: /^Play / }))
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
      await user.click(within(second[47]).getByRole('button', { name: /^Play / }))
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
      await user.click(within(rows[49]).getByRole('button', { name: /^Play / }))
      expect(screen.getByText('50 of 120')).toBeInTheDocument()

      screen.getByTestId('call-player').dispatchEvent(new Event('ended'))

      // The Run is on page two...
      expect(await screen.findByText('51 of 120')).toBeInTheDocument()
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
      await user.click(within(rows[0]).getByRole('button', { name: /^Play / }))

      await waitFor(() => expect(screen.getByText('1 of 3')).toBeInTheDocument())
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
      await user.click(within(rows[47]).getByRole('button', { name: /^Play / }))

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
      await user.click(within(rows[1]).getByRole('button', { name: /^Play / }))
      expect(screen.getByText('2 of 3')).toBeInTheDocument()

      await user.click(screen.getByRole('button', { name: 'Previous call' }))
      expect(screen.getByText('1 of 3')).toBeInTheDocument()
      expect(screen.getByTestId('call-player')).toHaveAttribute(
        'src',
        '/api/call/3/audio',
      )

      await user.click(screen.getByRole('button', { name: 'Next call' }))
      expect(screen.getByText('2 of 3')).toBeInTheDocument()
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
      await user.click(within(rows[0]).getByRole('button', { name: /^Play / }))

      await user.click(screen.getByRole('button', { name: 'Pause' }))
      // Still the current Call, still on the same source — paused, not dropped.
      expect(screen.getByText('1 of 3')).toBeInTheDocument()
      expect(screen.getByTestId('call-player')).toHaveAttribute(
        'src',
        '/api/call/3/audio',
      )

      await user.click(screen.getByRole('button', { name: 'Resume' }))
      expect(
        screen.getByRole('button', { name: 'Pause' }),
      ).toBeInTheDocument()
    })

    it('stops playing on demand', async () => {
      const user = userEvent.setup()
      renderApp('/search')

      await user.click(
        await screen.findByRole('button', { name: 'Playback mode' }),
      )
      const rows = await resultRows()
      await user.click(within(rows[0]).getByRole('button', { name: /^Play / }))
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
      await user.click(within(rows[1]).getByRole('button', { name: /^Play / }))

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

    await user.click(within(row).getByRole('button', { name: /^Play / }))
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
