import '@/index.css'

import axe from 'axe-core'
import { getResponse, HttpHandler } from 'msw'
import { page } from 'vitest/browser'
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest'

import { advance, received, turnFeedOff } from '@/store/live'
import { makeStore, type AppStore } from '@/store/store'
import { ARCHIVE, ORIGIN, UNIT_HISTORY, handlers } from '@/test/handlers'
import { renderApp } from '@/test/utils'

/**
 * Every Listener screen, measured at phone width with the real stylesheet
 * (#78).
 *
 * The critique pass that opened this ticket measured a live instance at
 * 390×760 and found 32-pixel controls, 9-pixel type, 12-pixel form fields that
 * make iOS zoom, and grey-on-grey that fails AA on five screens of six. None of
 * it is visible to jsdom, which has class names and no stylesheet; all of it is
 * a number here. So this is the same pass, kept: a regression is a named
 * control and its size, not a feeling that the app has got fiddly again.
 *
 * The bars, after Apple's and WCAG's:
 *
 * - **44 × 44** for every control a thumb has to land on. An inline text link
 *   is exempt (WCAG 2.5.8's own exception) — it is part of a sentence, and
 *   making it 44 tall would break the sentence rather than help the thumb.
 * - **11px** for any text a control carries.
 * - **16px** in every form field, the size under which iOS Safari zooms the
 *   whole page when one is focused.
 * - **AA contrast** (axe-core's `color-contrast`) on everything drawn.
 *
 * No MSW worker runs here (`setup.browser.ts`), so `fetch` is answered by the
 * very handlers the jsdom suite uses, through MSW's own `getResponse` — the
 * same fixtures at the same boundary, without a service worker in the way.
 */

/** The HTTP half of the shared handlers — `getResponse` resolves requests,
 *  and the live-feed socket's handler is not one. */
const HTTP = handlers.filter((handler) => handler instanceof HttpHandler)

/** Requests no fixture answered — each one a screen measured on data it never
 *  got, which is the jsdom suite's `onUnhandledRequest: 'error'` here. */
const unanswered: string[] = []

beforeAll(async () => {
  await page.viewport(390, 844)
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    const asked = new Request(input, init)
    const url = new URL(asked.url)
    // The fixtures answer at the jsdom origin; the page here has its own.
    const request = new Request(`${ORIGIN}${url.pathname}${url.search}`, asked)
    const answer = await getResponse(HTTP, request)
    if (answer) return answer
    unanswered.push(`${asked.method} ${url.pathname}`)
    return new Response('no fixture answers this\n', { status: 404 })
  })
})

afterEach(() => {
  expect(unanswered.splice(0)).toEqual([])
})

afterAll(() => {
  vi.unstubAllGlobals()
})

/** A control's name, for a failure message a person can act on. */
function nameOf(element: Element): string {
  const named =
    element.getAttribute('aria-label') ??
    (element as HTMLElement).innerText ??
    element.getAttribute('placeholder') ??
    element.tagName
  return `${element.tagName.toLowerCase()} "${named.trim().replace(/\s+/g, ' ').slice(0, 40)}"`
}

const CONTROLS =
  'button, a[href], select, input:not([type="hidden"]), textarea, summary, [role="button"], [role="switch"], [role="tab"]'

/** Everything on screen a thumb has to land on, and is drawn. */
function controlsIn(root: Element): Element[] {
  return [...root.querySelectorAll(CONTROLS)].filter((element) => {
    const box = element.getBoundingClientRect()
    return box.width > 0 && box.height > 0
  })
}

/** Controls a thumb cannot reliably hit — inline links excepted (WCAG 2.5.8). */
function smallTargets(root: Element): string[] {
  return controlsIn(root)
    .filter((element) => getComputedStyle(element).display !== 'inline')
    .map((element) => [element, element.getBoundingClientRect()] as const)
    .filter(([, box]) => Math.round(box.width) < 44 || Math.round(box.height) < 44)
    .map(([element, box]) => `${nameOf(element)} ${Math.round(box.width)}x${Math.round(box.height)}`)
}

/** Text inside a control drawn under 11px. */
function smallType(root: Element): string[] {
  const found = new Set<string>()
  for (const control of controlsIn(root)) {
    for (const element of [control, ...control.querySelectorAll('*')]) {
      const hasText = [...element.childNodes].some(
        (node) => node.nodeType === Node.TEXT_NODE && node.textContent?.trim(),
      )
      const size = parseFloat(getComputedStyle(element).fontSize)
      if (hasText && size < 11) found.add(`${nameOf(control)} at ${size}px`)
    }
  }
  return [...found]
}

/** Form fields under 16px — each one a page iOS Safari zooms on focus. */
function zoomingFields(root: Element): string[] {
  return [...root.querySelectorAll('input:not([type="range"]):not([type="hidden"]), select, textarea')]
    .filter((field) => parseFloat(getComputedStyle(field).fontSize) < 16)
    .map((field) => `${nameOf(field)} at ${getComputedStyle(field).fontSize}`)
}

/** What axe-core, reading real colours, says fails AA. */
async function lowContrast(root: Element): Promise<string[]> {
  const { violations } = await axe.run(root, { runOnly: ['color-contrast'] })
  return violations.flatMap(({ nodes }) =>
    nodes.map(({ target, any }) => {
      const data = (any[0]?.data ?? {}) as { contrastRatio?: number; fontSize?: string }
      return `${target.join(' ')} ${data.contrastRatio}:1 at ${data.fontSize}`
    }),
  )
}

/** Wait for `selector` to be drawn in `root`, and hand it over — polled, as
 *  this layer waits for everything (`setup.browser.ts`): a screen is measured
 *  once it has drawn what it is measured on, because an empty one passes every
 *  bar. */
async function drawn(root: ParentNode, selector: string): Promise<Element> {
  await expect.poll(() => root.querySelector(selector)).not.toBeNull()
  return root.querySelector(selector)!
}

/** The app at `route` on a phone, with a Call on the air so every control
 *  that acts on one is drawn live — returned once the screen has drawn what
 *  [`DRAWN`] says it is about. */
async function onAPhone(route: string): Promise<{ store: AppStore; root: Element }> {
  const store = makeStore({ storage: undefined })
  const { container } = renderApp(route, store)
  store.dispatch(received({ ...ARCHIVE[0], timestamp: Date.now() }, 1))
  store.dispatch(received({ ...ARCHIVE[1], timestamp: Date.now() }, 2))
  store.dispatch(received({ ...ARCHIVE[2], timestamp: Date.now() }, 3))
  // One finished, so RECENT has a row and the queue still has one waiting.
  store.dispatch(advance())
  await drawn(container, DRAWN[route])
  return { store, root: container }
}

/** A radio the fixtures have a history for (`UNIT_HISTORY`). */
const UNIT_ROUTE = `/unit/${UNIT_HISTORY.systemRef}/${UNIT_HISTORY.ref}`

/** What each screen must have drawn for its measurements to mean anything — a
 *  screen still waiting on its data passes every bar by being empty. */
const DRAWN: Record<string, string> = {
  '/': 'section[aria-label="Scanner display"]',
  '/talkgroups': 'main ul li button',
  '/search': 'ul[aria-label="Search results"] li',
  '/settings': 'main li',
  '/session': 'ul[aria-label="Session log"] li',
  '/dvr': 'main select',
  [UNIT_ROUTE]: 'main ul li',
}

describe('at phone width (#78)', () => {
  describe.each([
    ['Live', '/'],
    ['Talkgroups', '/talkgroups'],
    ['Search', '/search'],
    ['Settings', '/settings'],
    ['the session log', '/session'],
    ['the DVR', '/dvr'],
    ['a radio\'s history', UNIT_ROUTE],
  ])('%s', (_screen, route) => {
    it('gives every control a 44 × 44 target', async () => {
      const { root } = await onAPhone(route)
      expect(smallTargets(root)).toEqual([])
    })

    it('draws no control text under 11px', async () => {
      const { root } = await onAPhone(route)
      expect(smallType(root)).toEqual([])
    })

    it('sets every form field at 16px, so iOS does not zoom', async () => {
      const { root } = await onAPhone(route)
      expect(zoomingFields(root)).toEqual([])
    })

    it('passes AA contrast', async () => {
      const { root } = await onAPhone(route)
      expect(await lowContrast(root)).toEqual([])
    })

    it('never scrolls sideways', async () => {
      const { root } = await onAPhone(route)
      expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(390)
      expect(root.scrollWidth).toBeLessThanOrEqual(390)
    })
  })
})

/** The measurement that opened the Search half of the ticket: the first
 *  result at y=755 of 760, under ten filters. */
describe('Search, folded on a phone and open on a desk (#78)', () => {
  it('shows its first result on a phone without scrolling', async () => {
    const { root } = await onAPhone('/search')
    const first = root.querySelector('ul[aria-label="Search results"] li')!
    const tabBar = root.querySelector('nav[aria-label="Primary"]')!

    expect(first.getBoundingClientRect().top).toBeLessThan(tabBar.getBoundingClientRect().top)
  })

  it('lays the whole form open where there is room for it', async () => {
    await page.viewport(1024, 800)
    try {
      const { root } = await onAPhone('/search')
      const form = root.querySelector('#archive-filters')!
      const toggle = [...root.querySelectorAll('button')].find((button) =>
        button.textContent?.startsWith('Filters'),
      )!

      expect(getComputedStyle(form).display).toBe('grid')
      expect(getComputedStyle(toggle).display).toBe('none')
    } finally {
      await page.viewport(390, 844)
    }
  })
})

/** The sheets a phone opens — the queue, and a search row's `⋯`. */
describe('sheets at phone width (#78)', () => {
  it('gives every control in the queue sheet a 44 × 44 target', async () => {
    const { root } = await onAPhone('/')
    ;(root.querySelector('button[aria-label^="Queued calls"]') as HTMLButtonElement).click()
    const sheet = await drawn(document, '[role="dialog"]')

    expect(smallTargets(sheet)).toEqual([])
    expect(smallType(sheet)).toEqual([])
  })

  it("gives every control in a search row's sheet a 44 × 44 target", async () => {
    const { root } = await onAPhone('/search')
    ;(root.querySelector('button[aria-label^="More for"]') as HTMLButtonElement).click()
    const sheet = await drawn(document, '[role="dialog"]')

    expect(smallTargets(sheet)).toEqual([])
    expect(smallType(sheet)).toEqual([])
    expect(await lowContrast(sheet)).toEqual([])
  })
})

/** The desk layout (#78, brief item 29), laid out for real. */
describe('on a desk (#78)', () => {
  it('puts the tabs down the left and the Live scanner to the right of the screen', async () => {
    await page.viewport(1280, 800)
    try {
      const { root } = await onAPhone('/search')
      const nav = root.querySelector('nav[aria-label="Primary"]')!.getBoundingClientRect()
      const main = root.querySelector('main')!.getBoundingClientRect()
      const live = root.querySelector('aside[aria-label="Live"]')!.getBoundingClientRect()

      expect(nav.right).toBeLessThanOrEqual(main.left)
      expect(live.left).toBeGreaterThanOrEqual(main.right)
      // All three on screen at once, side by side, and nothing off its edge.
      expect(live.right).toBeLessThanOrEqual(1280)
      expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(1280)
      expect(smallTargets(root.querySelector('aside')!)).toEqual([])
      expect(await lowContrast(root)).toEqual([])
    } finally {
      await page.viewport(390, 844)
    }
  })
})

/** The strip's other mode — why nothing is playing, and the one tap back —
 *  which a playing strip never draws (#78). */
describe('the strip, quiet (#78)', () => {
  it('gives its way back a 44 × 44 target', async () => {
    const store = makeStore({ storage: undefined })
    const { container } = renderApp('/talkgroups', store)
    store.dispatch(turnFeedOff())
    const strip = await drawn(container, 'section[aria-label="Mini player"]')

    expect(smallTargets(strip)).toEqual([])
  })
})
