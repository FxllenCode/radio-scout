import { act, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { received } from '@/store/live'
import { makeStore } from '@/store/store'
import { renderApp } from '@/test/utils'
import type { Call } from '@/types'

const CALL: Call = {
  id: 1,
  systemRef: 11,
  systemLabel: 'Fulton County',
  talkgroupRef: 54241,
  talkgroupLabel: 'FD Dispatch',
  audioUrl: '/api/call/1/audio',
}

/** A browser as wide as a desk: every `min-width` query up to `px` matches.
 *  jsdom's own `matchMedia` matches nothing, which is a phone. */
function aDeskOf(px: number) {
  vi.stubGlobal('matchMedia', (query: string) => {
    const min = /min-width:\s*(\d+)px/.exec(query)
    return {
      matches: min !== null && px >= Number(min[1]),
      media: query,
      addEventListener() {},
      removeEventListener() {},
    } as unknown as MediaQueryList
  })
}

afterEach(() => vi.unstubAllGlobals())

const livePanel = () => screen.queryByRole('complementary', { name: 'Live' })

/**
 * The desktop layout (#78, design brief item 29): a phone's column with a
 * bottom tab bar, centred on a 1500-pixel screen, was what a desk got. From
 * 1024 pixels the tabs are a side nav, and the Live scanner stays beside
 * whatever else the Listener is looking at.
 */
describe('on a desk (#78)', () => {
  it('keeps the Live scanner beside the other screens, controls and all', () => {
    aDeskOf(1280)
    const store = makeStore()
    renderApp('/search', store)
    act(() => void store.dispatch(received(CALL, 1)))

    const panel = livePanel()!
    expect(within(panel).getByRole('region', { name: 'Scanner display' })).toHaveTextContent(
      'FD Dispatch',
    )
    expect(within(panel).getByRole('button', { name: 'Skip' })).toBeEnabled()
    // One page, one top-level heading: the screen's own.
    expect(screen.getAllByRole('heading', { level: 1 })).toHaveLength(1)
    expect(within(panel).getByRole('heading', { level: 2, name: 'LIVE' })).toBeInTheDocument()
  })

  /** The panel *is* the player there, so the strip would only repeat it. */
  it('drops the mini-player the panel makes redundant', () => {
    aDeskOf(1280)
    const store = makeStore()
    renderApp('/talkgroups', store)
    act(() => void store.dispatch(received(CALL, 1)))

    expect(screen.queryByRole('region', { name: 'Mini player' })).toBeNull()
  })

  it('does not draw the scanner twice on the Live screen itself', () => {
    aDeskOf(1280)
    const store = makeStore()
    renderApp('/', store)
    act(() => void store.dispatch(received(CALL, 1)))

    expect(livePanel()).toBeNull()
    expect(screen.getAllByRole('region', { name: 'Scanner display' })).toHaveLength(1)
  })

  it('turns the tabs into a side nav', () => {
    aDeskOf(1280)
    renderApp('/search')

    expect(screen.getByRole('navigation', { name: 'Primary' })).toHaveAttribute(
      'data-layout',
      'side',
    )
  })

  /** A phone keeps exactly what it had. */
  it('changes nothing on a phone', () => {
    const store = makeStore()
    renderApp('/talkgroups', store)
    act(() => void store.dispatch(received(CALL, 1)))

    expect(livePanel()).toBeNull()
    expect(screen.getByRole('region', { name: 'Mini player' })).toBeInTheDocument()
    expect(screen.getByRole('navigation', { name: 'Primary' })).toHaveAttribute(
      'data-layout',
      'bottom',
    )
  })
})

/**
 * Moving between tabs, said (#78). A click on a tab used to leave a screen
 * reader on the tab it pressed, with nothing to say the page under it had
 * changed; focus now lands on the new screen's heading.
 */
describe('changing tabs (#78)', () => {
  it("lands on the new screen's heading", async () => {
    const user = userEvent.setup()
    renderApp('/')

    await user.click(screen.getByRole('link', { name: 'Settings' }))

    expect(await screen.findByRole('heading', { level: 1, name: 'Settings' })).toHaveFocus()
  })

  it('takes no focus on the way in', () => {
    renderApp('/settings')

    expect(screen.getByRole('heading', { level: 1, name: 'Settings' })).not.toHaveFocus()
  })
})
