import { act, screen, within } from '@testing-library/react'
import { Profiler } from 'react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { axe } from 'vitest-axe'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { EVERYTHING } from '@/lib/selection'
import { avoid, received, selectSelection } from '@/store/live'
import { makeStore, type AppStore } from '@/store/store'
import { progressed } from '@/store/transport'

import { TalkgroupsScreen } from './TalkgroupsScreen'
import { ORIGIN } from '@/test/handlers'
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

/** The panel, and a store to inspect afterwards. */
function showPanel(store: AppStore = scannerStore()) {
  return { store, ...renderApp('/talkgroups', store) }
}

/** Wait for the catalog to arrive — every assertion below is downstream of it.
 *  Matched by prefix because an avoided row's accessible name carries the avoid
 *  after the label, which is the point of announcing it there. */
const talkgroupRow = (label: string) =>
  screen.findByRole('switch', { name: new RegExp(`^${label}`) })

const summary = () => screen.getByTestId('selection-summary')

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
      http.get(`${ORIGIN}/api/catalog`, () => HttpResponse.json({ systems: [] })),
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
  it('does not redraw because audio is playing', async () => {
    let commits = 0
    const store = scannerStore()
    renderWithProviders(
      <Profiler id="talkgroups" onRender={() => void (commits += 1)}>
        <TalkgroupsScreen />
      </Profiler>,
      { store },
    )
    await talkgroupRow('Alpha Fire')
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
})
