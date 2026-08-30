import { act, screen, within } from '@testing-library/react'
import { Profiler } from 'react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { axe } from 'vitest-axe'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { EVERYTHING } from '@/lib/selection'
import { avoid, received, selectSelection } from '@/store/live'
import { selectPinned, showSystem } from '@/store/panel'
import { makeStore, type AppStore } from '@/store/store'
import { progressed } from '@/store/transport'

import { TalkgroupsScreen } from './TalkgroupsScreen'
import { countyCatalog, ORIGIN } from '@/test/handlers'
import { server } from '@/test/setup'
import { renderApp, renderWithProviders } from '@/test/utils'

/** A store with storage of its own, so no test can inherit another's
 *  selection (and none depends on jsdom having local storage at all). */
function scannerStore(): AppStore {
  const map = new Map<string, string>()
  return makeStore({
    namespace: 'test',
    storage: {
      get length() {
        return map.size
      },
      clear: () => map.clear(),
      getItem: (key) => map.get(key) ?? null,
      key: (index) => [...map.keys()][index] ?? null,
      removeItem: (key) => void map.delete(key),
      setItem: (key, value) => void map.set(key, value),
    },
  })
}

/** The panel, and a store to inspect afterwards — which `renderApp` already
 *  hands back, so naming it again only shadowed it with itself. */
function showPanel(store: AppStore = scannerStore()) {
  return renderApp('/talkgroups', store)
}

/** Wait for the catalog to arrive — every assertion below is downstream of it.
 *  Matched by prefix because an avoided row's accessible name carries the avoid
 *  after the label, which is the point of announcing it there. */
const talkgroupRow = (label: string) =>
  screen.findByRole('switch', { name: new RegExp(`^${label}`) })

const summary = () => screen.getByTestId('selection-summary')

/** Tell every windowed list it sits `top` pixels from the top of the viewport,
 *  and let it notice. jsdom lays nothing out — every rect is zeros — so a
 *  scroll position is something a test states rather than performs. */
function scrolledTo(top: number) {
  vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue({
    top,
  } as DOMRect)
  act(() => {
    window.dispatchEvent(new Event('scroll'))
  })
}

/** A store with Talkgroup 1 of System 100 avoided until `until`. */
function withAvoided(until: number): AppStore {
  const store = scannerStore()
  store.dispatch(
    received({ id: 1, systemRef: 100, talkgroupRef: 1, audioUrl: '/api/call/1/audio' }, 1),
  )
  store.dispatch(avoid({ until }))
  return store
}

describe('TalkgroupsScreen (#12, spec US 19–22)', () => {
  it('lists every System with the Talkgroups under it', async () => {
    showPanel()

    expect(await talkgroupRow('Alpha Fire')).toBeInTheDocument()
    const alpha = screen.getByRole('group', { name: /Alpha/ })
    expect(within(alpha).getAllByRole('switch')).toHaveLength(3)
    expect(
      within(screen.getByRole('group', { name: /Beta/ })).getAllByRole('switch'),
    ).toHaveLength(1)
  })

  it('starts with everything on, and says so', async () => {
    showPanel()

    expect(await talkgroupRow('Alpha Fire')).toBeChecked()
    expect(summary()).toHaveTextContent('4 of 4 on')
  })

  it('turns one Talkgroup off (spec US 19)', async () => {
    const { store } = showPanel()

    await userEvent.click(await talkgroupRow('Alpha Fire'))

    expect(await talkgroupRow('Alpha Fire')).not.toBeChecked()
    expect(summary()).toHaveTextContent('3 of 4 on')
    expect(selectSelection(store.getState())).toEqual({
      all: true,
      sel: { '100': { '1': false } },
    })
  })

  /** A Group spans Systems — Public covers Alpha Law, Alpha Quiet and Beta
   *  Dispatch — which is the point of a category toggle (spec US 20). */
  it('toggles a Group category across Systems', async () => {
    const { store } = showPanel()
    const publicGroup = await screen.findByRole('button', { name: /^Public/ })
    expect(publicGroup).toHaveAttribute('aria-pressed', 'true')

    await userEvent.click(publicGroup)

    expect(await talkgroupRow('Alpha Law')).not.toBeChecked()
    expect(await talkgroupRow('Beta Dispatch')).not.toBeChecked()
    expect(await talkgroupRow('Alpha Fire')).toBeChecked()
    expect(summary()).toHaveTextContent('1 of 4 on')
    expect(selectSelection(store.getState()).all).toBe(true)
  })

  it('shows a partly-selected category as mixed', async () => {
    showPanel()

    await userEvent.click(await talkgroupRow('Alpha Law'))

    expect(screen.getByRole('button', { name: /^Public/ })).toHaveAttribute(
      'aria-pressed',
      'mixed',
    )
    expect(screen.getByRole('button', { name: /^Emergency/ })).toHaveAttribute(
      'aria-pressed',
      'mixed',
    )
  })

  /** rdio's category buttons are built from Groups alone — its Tag category
   *  type is never constructed. Spec US 20 asks for both. */
  it('toggles a Tag category as well as a Group one', async () => {
    showPanel()
    await userEvent.click(await screen.findByRole('button', { name: /^Fire/ }))

    expect(await talkgroupRow('Alpha Fire')).not.toBeChecked()
    expect(await talkgroupRow('Beta Dispatch')).not.toBeChecked()
    expect(await talkgroupRow('Alpha Law')).toBeChecked()
  })

  it('turns a whole System off and back on (spec US 21)', async () => {
    const { store } = showPanel()
    await talkgroupRow('Alpha Fire')
    const alpha = screen.getByRole('group', { name: /Alpha/ })

    await userEvent.click(within(alpha).getByRole('button', { name: /all off/i }))

    expect(await talkgroupRow('Alpha Fire')).not.toBeChecked()
    expect(await talkgroupRow('Beta Dispatch')).toBeChecked()
    expect(selectSelection(store.getState())).toEqual({
      all: true,
      sel: { '100': { '*': false } },
    })

    await userEvent.click(within(alpha).getByRole('button', { name: /all on/i }))
    expect(selectSelection(store.getState())).toEqual(EVERYTHING)
  })

  it('turns everything off and back on (spec US 21)', async () => {
    const { store } = showPanel()
    await talkgroupRow('Alpha Fire')

    await userEvent.click(screen.getByRole('button', { name: 'Turn everything off' }))

    expect(summary()).toHaveTextContent('0 of 4 on')
    expect(selectSelection(store.getState())).toEqual({ all: false, sel: {} })

    await userEvent.click(screen.getByRole('button', { name: 'Turn everything on' }))
    expect(selectSelection(store.getState())).toEqual(EVERYTHING)
  })

  describe('avoided Talkgroups', () => {
    beforeEach(() => vi.useFakeTimers({ shouldAdvanceTime: true }))
    afterEach(() => vi.useRealTimers())

    /** The blink state the brief asks for: a timed avoid (spec US 14) is a
     *  Talkgroup coming back on its own, which is not the same as one the
     *  listener switched off. */
    it('marks a timed avoid with how long it has left', async () => {
      vi.setSystemTime(0)
      showPanel(withAvoided(30 * 60_000))

      const row = await talkgroupRow('Alpha Fire')
      expect(within(row).getByText('30m')).toBeInTheDocument()
      expect(row).not.toBeChecked()
    })

    it('marks an indefinite avoid as avoided rather than as a countdown', async () => {
      vi.setSystemTime(0)
      showPanel(withAvoided(0))

      expect(within(await talkgroupRow('Alpha Fire')).getByText('avoid')).toBeVisible()
    })

    /** The panel says what the listener will hear. A Talkgroup that is muted
     *  reads off in the summary and in the categories too, or the panel would
     *  claim "4 of 4 on" over a visibly-off row. */
    it('counts an avoided Talkgroup as off, everywhere', async () => {
      vi.setSystemTime(0)
      showPanel(withAvoided(0))
      await talkgroupRow('Alpha Fire')

      expect(summary()).toHaveTextContent('3 of 4 on')
      expect(screen.getByRole('button', { name: /^Fire/ })).toHaveAttribute(
        'aria-pressed',
        'mixed',
      )
      expect(screen.getByRole('button', { name: /^Emergency/ })).toHaveAttribute(
        'aria-pressed',
        'mixed',
      )
    })

    it('lets every avoided Talkgroup back in on "all on"', async () => {
      vi.setSystemTime(0)
      const store = withAvoided(0)
      showPanel(store)
      await talkgroupRow('Alpha Fire')

      await userEvent.click(screen.getByRole('button', { name: 'Turn everything on' }))

      expect(summary()).toHaveTextContent('4 of 4 on')
      expect(await talkgroupRow('Alpha Fire')).toBeChecked()
    })

    it('blinks a timed avoid and holds an indefinite one steady', async () => {
      vi.setSystemTime(0)
      const { unmount } = showPanel(withAvoided(30 * 60_000))
      expect(
        within(await talkgroupRow('Alpha Fire')).getByTestId('avoid-blink'),
      ).toBeInTheDocument()
      unmount()

      showPanel(withAvoided(0))
      expect(
        within(await talkgroupRow('Alpha Fire')).queryByTestId('avoid-blink'),
      ).toBeNull()
    })

    /** A countdown that doesn't count down is a lie the listener can read. */
    it('counts the avoid down as its time passes', async () => {
      vi.setSystemTime(0)
      showPanel(withAvoided(30 * 60_000))
      expect(within(await talkgroupRow('Alpha Fire')).getByText('30m')).toBeVisible()

      await act(async () => {
        vi.advanceTimersByTime(10 * 60_000)
      })

      expect(within(await talkgroupRow('Alpha Fire')).getByText('20m')).toBeVisible()
    })

    it('lets an avoided Talkgroup back in when it is switched on again', async () => {
      vi.setSystemTime(0)
      const store = withAvoided(0)
      showPanel(store)

      await userEvent.click(await talkgroupRow('Alpha Fire'))

      expect(await talkgroupRow('Alpha Fire')).toBeChecked()
      expect(selectSelection(store.getState())).toEqual(EVERYTHING)
    })
  })

  /** rdio renders one flat wall of buttons per System; on a real
   *  300-Talkgroup system that is a scroll, not a choice. */
  it('filters the list by name or TGID', async () => {
    showPanel()
    await talkgroupRow('Alpha Fire')
    const filter = screen.getByRole('searchbox', { name: /filter/i })

    await userEvent.type(filter, 'law')

    expect(screen.getByRole('switch', { name: /Alpha Law/ })).toBeInTheDocument()
    expect(screen.queryByRole('switch', { name: /Alpha Fire/ })).not.toBeInTheDocument()
    expect(screen.queryByRole('group', { name: /Beta/ })).not.toBeInTheDocument()

    await userEvent.clear(filter)
    await userEvent.type(filter, '3')
    expect(screen.getByRole('switch', { name: /Alpha Quiet/ })).toBeInTheDocument()
  })

  /** The button acts on what it is next to. With a filter typed, "All off" on
   *  a System showing one row must not silence the thirty-nine it is hiding. */
  it('applies a System’s all-off to what the filter is showing', async () => {
    const { store } = showPanel()
    await talkgroupRow('Alpha Fire')
    await userEvent.type(screen.getByRole('searchbox', { name: /filter/i }), 'law')

    await userEvent.click(
      within(screen.getByRole('group', { name: /Alpha/ })).getByRole('button', {
        name: /all off/i,
      }),
    )

    expect(selectSelection(store.getState())).toEqual({
      all: true,
      sel: { '100': { '2': false } },
    })
  })

  it('says so when a filter matches nothing', async () => {
    showPanel()
    await talkgroupRow('Alpha Fire')

    await userEvent.type(screen.getByRole('searchbox', { name: /filter/i }), 'zzz')

    expect(screen.getByText(/no talkgroups match/i)).toBeInTheDocument()
  })

  /** A recorder that sends no labels, or a curated row with them cleared: the
   *  panel still has to be usable, so Refs stand in for names and a catalog
   *  with nothing to categorize by simply shows no category rows. */
  it('names an unlabeled System and Talkgroup by their Refs', async () => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () =>
        HttpResponse.json({
          activityWindowMs: 24 * 60 * 60 * 1_000,
          systems: [{ ref: 42, talkgroups: [{ ref: 7, groups: [] }] }],
        }),
      ),
    )
    showPanel()

    expect(await talkgroupRow('Talkgroup 7')).toBeInTheDocument()
    expect(screen.getByRole('group', { name: /System 42/ })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /^Groups/ })).not.toBeInTheDocument()
    expect(screen.queryByLabelText('Tags')).not.toBeInTheDocument()
  })

  it('invites a recorder when there is nothing to select yet', async () => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () => HttpResponse.json({ systems: [], activityWindowMs: 24 * 60 * 60 * 1_000 })),
    )
    showPanel()

    expect(await screen.findByText('No systems yet.')).toBeInTheDocument()
    expect(screen.queryByTestId('selection-summary')).not.toBeInTheDocument()
  })

  it('admits it when the catalog cannot be loaded', async () => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () => new HttpResponse('boom', { status: 500 })),
    )
    showPanel()

    expect(await screen.findByRole('alert')).toHaveTextContent(/could not load/i)
  })

  /**
   * #57 asks that 400+ rows scroll smoothly *while audio plays*, and #91 is
   * what makes that reachable. Playback progress lands several times a second,
   * so anything the panel reads that is rebuilt per dispatch — an unmemoized
   * matrix, a per-row store subscription, a fresh derivation — redraws every
   * row several times a second precisely *because* a Call is playing.
   *
   * Counted as commits rather than asserted about the selectors, so it stays
   * true of whatever the panel is made of later: a future inline object
   * anywhere in this subtree fails here.
   */
  it('does not redraw a 400-row panel because audio is playing', async () => {
    server.use(
      http.get(`${ORIGIN}/api/catalog`, () => HttpResponse.json(countyCatalog(400))),
    )
    let commits = 0
    const store = scannerStore()
    store.dispatch(showSystem({ systemRef: 1, shown: true }))
    renderWithProviders(
      <Profiler id="talkgroups" onRender={() => void (commits += 1)}>
        <TalkgroupsScreen />
      </Profiler>,
      { store },
    )
    await screen.findByRole('switch', { name: /Channel 001/ })
    const drawn = commits

    act(() => {
      store.dispatch(
        received({ id: 7, systemRef: 100, talkgroupRef: 2, audioUrl: '/api/call/7/audio' }, 7),
      )
      for (const position of [0.5, 1, 1.5, 2]) {
        store.dispatch(progressed({ position, duration: 9 }))
      }
    })

    expect(commits).toBe(drawn)
  })

  it('has no accessibility violations', async () => {
    const { container } = showPanel()
    await talkgroupRow('Alpha Fire')

    expect(await axe(container)).toHaveNoViolations()
  })

  // -------------------------------------------------------------------------
  // County scale (#57): 400+ rows that still behave like a panel.
  // -------------------------------------------------------------------------

  /** Serve a County of `rows` Talkgroups, and open the System that holds them —
   *  at this size it starts folded away, which is the point. The fixture runs
   *  activity backwards against catalog order (`countyCatalog`), so nothing
   *  below can pass against a sort that does nothing. */
  function showCounty(rows: number, opened = true) {
    // Built now rather than when the request lands: the ages on these rows are
    // measured against the screen's own clock, which starts at mount.
    const catalog = countyCatalog(rows)
    server.use(http.get(`${ORIGIN}/api/catalog`, () => HttpResponse.json(catalog)))
    const store = scannerStore()
    if (opened) store.dispatch(showSystem({ systemRef: 1, shown: true }))
    return showPanel(store)
  }

  const switches = () => screen.getAllByRole('switch')

  describe('at county scale', () => {
    /** The acceptance criterion, as the DOM can answer it: four hundred rows
     *  are not four hundred buttons. What is drawn is what is on screen and a
     *  little either side. */
    it('draws a windowful of a 400-row System, not 400 rows', async () => {
      showCounty(400)
      await screen.findByRole('switch', { name: /Channel 001/ })

      expect(switches().length).toBeGreaterThan(1)
      expect(switches().length).toBeLessThan(100)
      expect(screen.queryByRole('switch', { name: /Channel 400/ })).toBeNull()
    })

    /** ...and the rows it skipped are still *there*: the list stands its full
     *  height, so the page's scroll bar does not shrink as the Listener reads
     *  and the browser never yanks their position. */
    it('stands as tall as the whole System while drawing a slice of it', async () => {
      showCounty(400)
      await screen.findByRole('switch', { name: /Channel 001/ })
      const list = screen.getByRole('switch', { name: /Channel 001/ }).closest('ul')!

      const drawn = switches().length
      const padding =
        Number.parseInt(list.style.paddingTop || '0', 10) +
        Number.parseInt(list.style.paddingBottom || '0', 10)

      expect(padding + drawn * 44).toBe(400 * 44)
    })

    it('draws the rows the Listener has scrolled to', async () => {
      showCounty(400)
      await screen.findByRole('switch', { name: /Channel 001/ })

      // jsdom lays nothing out, so the list is told where it is: a hundred
      // rows' worth of scroll above the top of the viewport.
      scrolledTo(-100 * 44)

      expect(await screen.findByRole('switch', { name: /Channel 100/ })).toBeVisible()
      expect(screen.queryByRole('switch', { name: /Channel 001/ })).toBeNull()
    })

    it('folds a county-sized System away and opens it on a tap', async () => {
      showCounty(400, false)
      const header = await screen.findByRole('button', { name: 'County' })

      expect(header).toHaveAttribute('aria-expanded', 'false')
      expect(screen.queryByRole('switch')).toBeNull()
      // Its controls are still on screen, which is what folding buys.
      expect(screen.getByRole('button', { name: /Turn County all off/ })).toBeVisible()

      await userEvent.click(header)

      expect(header).toHaveAttribute('aria-expanded', 'true')
      expect(switches().length).toBeGreaterThan(0)
    })

    it('leaves a small System open', async () => {
      showPanel()
      await talkgroupRow('Alpha Fire')

      expect(screen.getByRole('button', { name: 'Alpha' })).toHaveAttribute(
        'aria-expanded',
        'true',
      )
    })

    /** The global controls are above the list rather than below it: at 400 rows
     *  an "All off" at the foot is a control that exists and cannot be
     *  reached. */
    it('keeps the global controls with the filter and the summary', async () => {
      showCounty(400)
      await screen.findByRole('switch', { name: /Channel 001/ })
      const bar = screen.getByRole('searchbox', { name: /filter/i }).closest('div')!
        .parentElement!

      expect(within(bar).getByRole('button', { name: 'Turn everything off' })).toBeVisible()
      expect(within(bar).getByText('400 of 400 on')).toBeVisible()
    })
  })

  describe('Pins (spec US 29)', () => {
    const pinnedSection = () => screen.getByRole('region', { name: 'Pinned' })

    it('holds a pinned Talkgroup at the top, and says which System it is from', async () => {
      const { store } = showPanel()
      await talkgroupRow('Beta Dispatch')

      await userEvent.click(screen.getByRole('button', { name: 'Pin Beta Dispatch' }))

      expect(selectPinned(store.getState())).toEqual(['200:1'])
      const pinned = within(pinnedSection()).getByRole('switch', { name: /Beta Dispatch/ })
      expect(pinned).toBeChecked()
      expect(within(pinnedSection()).getByText('Beta')).toBeVisible()
      // The panel's first list is the pinned one: it is drawn above every
      // System, which is the whole of what a Pin buys at 400 rows.
      expect(screen.getAllByRole('list')[0]).toBe(within(pinnedSection()).getByRole('list'))
    })

    /** A Pin is panel ordering and nothing else (CONTEXT.md), so the row is
     *  still in its System — and both copies act on the same Talkgroup. */
    it('leaves the row in its System, and either copy still switches it', async () => {
      const { store } = showPanel()
      await talkgroupRow('Beta Dispatch')
      await userEvent.click(screen.getByRole('button', { name: 'Pin Beta Dispatch' }))

      const beta = screen.getByRole('group', { name: /Beta/ })
      await userEvent.click(within(beta).getByRole('switch', { name: /Beta Dispatch/ }))

      expect(
        within(pinnedSection()).getByRole('switch', { name: /Beta Dispatch/ }),
      ).not.toBeChecked()
      expect(selectSelection(store.getState())).toEqual({
        all: true,
        sel: { '200': { '1': false } },
      })
    })

    it('unpins it again, and the section goes with the last Pin', async () => {
      showPanel()
      await talkgroupRow('Beta Dispatch')
      await userEvent.click(screen.getByRole('button', { name: 'Pin Beta Dispatch' }))

      await userEvent.click(
        within(pinnedSection()).getByRole('button', { name: 'Unpin Beta Dispatch' }),
      )

      expect(screen.queryByRole('region', { name: 'Pinned' })).toBeNull()
    })

    it('shows no pinned section when nothing is pinned', async () => {
      showPanel()
      await talkgroupRow('Alpha Fire')

      expect(screen.queryByRole('region', { name: 'Pinned' })).toBeNull()
    })
  })

  describe('activity on the row', () => {
    it('shows how long ago each Talkgroup was last heard', async () => {
      showCounty(3)

      // Channel 001 is the quietest of the three, and the longest silent.
      expect(await screen.findByRole('switch', { name: /Channel 001/ })).toHaveTextContent(
        '3m',
      )
      expect(screen.getByRole('switch', { name: /Channel 003 last heard 1m ago/ }))
        .toBeVisible()
    })

    it('says nothing about a Talkgroup the window did not hear', async () => {
      showPanel()

      expect(await talkgroupRow('Alpha Fire')).not.toHaveTextContent(/last heard/)
    })

    /** Sorted by activity, the row shows the count the order is *by* — an
     *  ordering nothing on screen explains is one a Listener reads as broken. */
    it('sorts by most active, and shows the count it sorted on', async () => {
      showCounty(3)
      await screen.findByRole('switch', { name: /Channel 001/ })

      await userEvent.click(
        screen.getByRole('button', { name: 'Sort by most active in the last 24h' }),
      )

      // Busiest first — the reverse of the catalog order they were drawn in a
      // moment ago, which is what makes this an assertion about the sort.
      expect(switches().map((row) => row.textContent)).toEqual([
        expect.stringContaining('Channel 003'),
        expect.stringContaining('Channel 002'),
        expect.stringContaining('Channel 001'),
      ])
      expect(screen.getByRole('switch', { name: /Channel 003 3 recent calls/ }))
        .toBeVisible()
      expect(screen.getByRole('switch', { name: /Channel 001 1 recent call$/ }))
        .toBeVisible()
    })

    it('remembers the sort', async () => {
      const { store, unmount } = showCounty(3)
      await screen.findByRole('switch', { name: /Channel 001/ })
      await userEvent.click(
        screen.getByRole('button', { name: /Sort by most active/ }),
      )
      unmount()

      showPanel(store)

      expect(
        await screen.findByRole('button', { name: /Sort by most active/ }),
      ).toHaveAttribute('aria-pressed', 'true')
    })
  })
})
