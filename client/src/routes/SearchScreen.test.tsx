import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { axe } from 'vitest-axe'
import { beforeEach, describe, expect, it } from 'vitest'

import { ARCHIVE, ORIGIN, archivePage } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp } from '@/test/utils'

/** Every `/api/calls` query string the screen sent, in order. */
let searches: string[] = []

beforeEach(() => {
  searches = []
  server.use(
    http.get(`${ORIGIN}/api/calls`, ({ request }) => {
      const url = new URL(request.url)
      searches.push(url.search)
      return HttpResponse.json(archivePage(url))
    }),
  )
})

const lastSearch = () => new URLSearchParams(searches.at(-1))

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
    expect(within(row).getByText('54241')).toBeInTheDocument()
    expect(within(row).getByText('11')).toBeInTheDocument()
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
        '54241',
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
