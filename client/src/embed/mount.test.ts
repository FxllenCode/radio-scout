import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { axe } from 'vitest-axe'

import { saveGrant } from '@/lib/access'
import { ORIGIN, liveFeed } from '@/test/handlers'
import { server } from '@/test/setup'
import type { Call } from '@/types'

import { mountEmbed, type Mounted } from './mount'
import type { EmbedFeed } from './player'

const TOKEN = 'a1b2c3'

function call(id: number, extra: Partial<Call> = {}): Call {
  return {
    id,
    systemRef: 11,
    systemLabel: 'County',
    talkgroupRef: 100,
    talkgroupLabel: `Fire ${id}`,
    timestamp: Date.now() - (100 - id) * 1000,
    durationMs: 4200,
    audioUrl: `/api/call/${id}/audio`,
    ...extra,
  }
}

const SELECTION = { all: false, sel: { 11: { 100: true } } }

function feedOf(...calls: Call[]): EmbedFeed {
  return { name: 'Fire dispatch', selection: SELECTION, calls }
}

/** How many times the page asked for its feed. */
let feedReads = 0
/** Every frame the page sent the socket, parsed. */
let sent: unknown[] = []
/** The socket URLs the page dialled. */
let dialled: string[] = []
/** The connected socket's server side, for pushing Calls down it. */
let socket: { send(data: string): void; close(): void } | undefined
/** Whether the page closed its socket — what the server sees. */
let closed = false

function answering(feed: EmbedFeed | (() => Response)) {
  server.use(
    http.get(`${ORIGIN}/api/embed`, ({ request }) => {
      feedReads += 1
      expect(new URL(request.url).searchParams.get('t')).toBe(TOKEN)
      return typeof feed === 'function' ? feed() : HttpResponse.json(feed)
    }),
  )
}

beforeEach(() => {
  feedReads = 0
  sent = []
  dialled = []
  socket = undefined
  closed = false
  server.use(
    liveFeed.addEventListener('connection', ({ client }) => {
      dialled.push(client.url.toString())
      socket = client
      client.addEventListener('close', () => {
        closed = true
      })
      client.addEventListener('message', (event) => {
        sent.push(JSON.parse(String(event.data)))
      })
      client.send(JSON.stringify({ t: 'hello', protocol: 2, heartbeatMs: 30_000 }))
    }),
  )
})

let mounted: Mounted | undefined
afterEach(() => {
  mounted?.destroy()
  mounted = undefined
  document.body.innerHTML = ''
})

function mount(token: string | null = TOKEN, retryMs = 5) {
  const root = document.createElement('main')
  document.body.append(root)
  mounted = mountEmbed(root, { token, retryMs, liveRetryMs: 5 })
  return root
}

/** Push one live Call down the socket, as the server would. */
function push(id: number, seq = id) {
  socket?.send(JSON.stringify({ t: 'call', seq, call: call(id) }))
}

function audio(root: HTMLElement): HTMLAudioElement {
  const element = root.querySelector('audio')
  if (!element) throw new Error('no audio element')
  return element
}

describe('the embed page', () => {
  it('shows the embed name and its recent calls', async () => {
    answering(feedOf(call(2), call(1)))
    mount()

    expect(
      await screen.findByRole('heading', { name: 'Fire dispatch' }),
    ).toBeInTheDocument()
    const rows = within(screen.getByRole('list', { name: 'Recent calls' }))
      .getAllByRole('listitem')
    expect(rows).toHaveLength(2)
    expect(rows[0]).toHaveTextContent('Fire 2')
    expect(rows[0]).toHaveTextContent('County')
    expect(rows[0]).toHaveTextContent('4.2s')
  })

  /** Loading costs one read and no socket: most readers never press play. */
  it('opens no socket until the reader presses listen', async () => {
    answering(feedOf(call(1)))
    mount()
    await screen.findByRole('heading', { name: 'Fire dispatch' })

    expect(dialled).toEqual([])
    expect(feedReads).toBe(1)
  })

  it('says when there is nothing yet', async () => {
    answering(feedOf())
    mount()

    expect(await screen.findByText('No calls yet.')).toBeInTheDocument()
  })

  /** A deleted embed is a sentence in the host's frame, not an error page. */
  it('says the feed is no longer available when the embed is gone', async () => {
    answering(() => new HttpResponse('embed not found\n', { status: 404 }))
    mount()

    expect(
      await screen.findByText('This feed is no longer available.'),
    ).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Listen live' })).toBeNull()
  })

  it('says so without asking when the frame names no embed at all', async () => {
    answering(feedOf(call(1)))
    mount(null)

    expect(
      await screen.findByText('This feed is no longer available.'),
    ).toBeInTheDocument()
    expect(feedReads).toBe(0)
  })

  /** **Unreachable degrades**: the page says it cannot reach the scanner, and
   *  keeps trying until it can. */
  it('keeps trying a feed that would not load, and shows it when it does', async () => {
    let failures = 2
    answering(() =>
      failures-- > 0
        ? new HttpResponse(null, { status: 502 })
        : HttpResponse.json(feedOf(call(1))),
    )
    mount()

    expect(
      await screen.findByText("Can't reach the scanner right now. Trying again…"),
    ).toBeInTheDocument()
    expect(
      await screen.findByRole('heading', { name: 'Fire dispatch' }),
    ).toBeInTheDocument()
    expect(feedReads).toBe(3)
  })

  it('keeps trying through a network error too', async () => {
    let failures = 1
    answering(() =>
      failures-- > 0 ? HttpResponse.error() : HttpResponse.json(feedOf(call(1))),
    )
    mount()

    expect(
      await screen.findByRole('heading', { name: 'Fire dispatch' }),
    ).toBeInTheDocument()
  })

  it('links to the full scanner, tuned to the same selection', async () => {
    answering(feedOf(call(1)))
    mount()

    const link = await screen.findByRole('link', { name: /Open in Radio-Scout/ })
    expect(link).toHaveAttribute('href', '/talkgroups?sel=0_11.100')
    expect(link).toHaveAttribute('target', '_blank')
    expect(link).toHaveAttribute('rel', 'noopener')
  })

  it('has no accessibility violations', async () => {
    answering(feedOf(call(2), call(1)))
    const root = mount()
    await screen.findByRole('heading', { name: 'Fire dispatch' })

    expect(await axe(root)).toHaveNoViolations()
  })
})

describe('listening live', () => {
  /** Pressing listen subscribes the ordinary live feed to the embed's own
   *  Selection — and asks for the feed again, so the list is current from the
   *  moment listening starts. */
  it('subscribes to the embed selection when the reader presses listen', async () => {
    answering(feedOf(call(1)))
    mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))

    await waitFor(() => expect(sent).toEqual([{ t: 'sub', ...SELECTION }]))
    expect(feedReads).toBe(2)
    expect(await screen.findByRole('status')).toHaveTextContent('Live')
    expect(screen.getByRole('button', { name: 'Stop' })).toBeInTheDocument()
  })

  /** The address is public, so the page is a Listener holding nothing — even
   *  on a browser that holds a grant for this Instance from the app. */
  it('carries no grant to the socket', async () => {
    saveGrant(globalThis.localStorage, `rsg_${'ab'.repeat(16)}`)
    answering(feedOf(call(1)))
    mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))

    await waitFor(() => expect(dialled).toHaveLength(1))
    expect(dialled[0]).not.toContain('grant')
  })

  it('plays each call as it arrives on the one audio element', async () => {
    answering(feedOf(call(1)))
    const root = mount()
    const play = vi.spyOn(HTMLMediaElement.prototype, 'play')
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    push(7)
    push(8)

    await waitFor(() => expect(audio(root).src).toBe(`${ORIGIN}/api/call/7/audio`))
    expect(play).toHaveBeenCalledTimes(1)
    const rows = within(screen.getByRole('list', { name: 'Recent calls' }))
      .getAllByRole('listitem')
    expect(rows.map((row) => row.textContent)).toEqual([
      expect.stringContaining('Fire 8'),
      expect.stringContaining('Fire 7'),
      expect.stringContaining('Fire 1'),
    ])

    audio(root).dispatchEvent(new Event('ended'))

    await waitFor(() => expect(audio(root).src).toBe(`${ORIGIN}/api/call/8/audio`))
    expect(play).toHaveBeenCalledTimes(2)
    expect(root.querySelectorAll('audio')).toHaveLength(1)
  })

  /** A reader who taps a row on an Instance that has gone away is told so —
   *  degrading gracefully while not listening, too. */
  it('says so when a tapped call will not play', async () => {
    answering(feedOf(call(2), call(1)))
    const root = mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Play Fire 1' }))

    audio(root).dispatchEvent(new Event('error'))

    expect(await screen.findByRole('status')).toHaveTextContent(
      "Couldn't play that call. The scanner may be unreachable.",
    )
    expect(screen.getByRole('button', { name: 'Play Fire 1' })).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Play Fire 2' }))

    expect(screen.getByRole('status')).toHaveTextContent('')
  })

  /** A Call whose audio fails is a Call to move past, not a stalled page. */
  it('moves on when a call will not play', async () => {
    answering(feedOf())
    const root = mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))
    await waitFor(() => expect(sent).toHaveLength(1))
    push(7)
    push(8)
    await waitFor(() => expect(audio(root).src).toContain('/api/call/7/audio'))

    audio(root).dispatchEvent(new Event('error'))

    await waitFor(() => expect(audio(root).src).toContain('/api/call/8/audio'))
  })

  it('stops, and lets go of the socket, when the reader presses stop', async () => {
    answering(feedOf())
    const root = mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))
    await waitFor(() => expect(sent).toHaveLength(1))
    push(7)
    await waitFor(() => expect(audio(root).src).toContain('/api/call/7/audio'))
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause')

    await userEvent.click(screen.getByRole('button', { name: 'Stop' }))

    expect(pause).toHaveBeenCalled()
    expect(screen.getByRole('button', { name: 'Listen live' })).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('')
    // Nothing more can be heard: the page let go of its socket. (A straggler
    // that beat the close is ignored — `player.test.ts` says so.)
    await waitFor(() => expect(closed).toBe(true))
  })

  /** **The Operator owns what it plays**: the feed is read again on Listen, and
   *  an embed re-scoped since the page loaded is what the socket then follows. */
  it('follows a re-scope it reads while listening', async () => {
    const rescoped = { all: true, sel: {} }
    let reads = 0
    answering(() =>
      HttpResponse.json(
        reads++ === 0 ? feedOf(call(1)) : { ...feedOf(call(1)), selection: rescoped },
      ),
    )
    mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))

    await waitFor(() =>
      expect(sent).toEqual([
        { t: 'sub', ...SELECTION },
        { t: 'sub', ...rescoped },
      ]),
    )
    expect(dialled).toHaveLength(1)
  })

  /** **Degrades after load**: a dropped Instance leaves the list alone and says
   *  it is reconnecting. */
  it('says it is reconnecting when the socket drops, and keeps the list', async () => {
    answering(feedOf(call(1)))
    mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    socket?.close()

    await waitFor(() =>
      expect(screen.getByRole('status')).toHaveTextContent('Reconnecting…'),
    )
    expect(screen.getByText('Fire 1')).toBeInTheDocument()
  })

  /** ...and when it is back, it asks for what it missed — the app's own
   *  **Backfill** cursor, the highest emission heard — and plays it. */
  it('catches up on what it missed when the socket comes back', async () => {
    answering(feedOf())
    const root = mount()
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))
    await waitFor(() => expect(sent).toHaveLength(1))
    push(7, 41)
    // Frames the page has no use for are taken quietly.
    socket?.send(JSON.stringify({ t: 'lagged', skipped: 3 }))
    socket?.send(JSON.stringify({ t: 'gap' }))
    await screen.findByText('Fire 7')

    socket?.close()

    await waitFor(() => expect(dialled).toHaveLength(2))
    await waitFor(() =>
      expect(sent[1]).toEqual({ t: 'sub', ...SELECTION, since: 41 }),
    )
    push(8, 42)
    await screen.findByText('Fire 8')
    expect(screen.getByRole('status')).toHaveTextContent('Live')
    expect(audio(root).src).toContain('/api/call/7/audio')
  })

  /** An embed the Operator deleted while a reader was on the page: pressing
   *  Listen reads the feed again, finds it gone, and the page says so — list,
   *  socket and all. */
  it('says the feed is gone when it is deleted under an open page', async () => {
    let deleted = false
    answering(() =>
      deleted
        ? new HttpResponse('embed not found\n', { status: 404 })
        : HttpResponse.json(feedOf(call(2), call(1))),
    )
    mount()
    const listen = await screen.findByRole('button', { name: 'Listen live' })
    deleted = true

    await userEvent.click(listen)

    expect(
      await screen.findByText('This feed is no longer available.'),
    ).toBeInTheDocument()
    expect(screen.queryByText('Fire 1')).toBeNull()
    expect(screen.queryByRole('listitem')).toBeNull()
  })

  /** A browser that refuses to play without a fresh press puts the page back
   *  to its button, so the reader's next press is the gesture it wants. */
  it('goes back to its button when the browser refuses to play', async () => {
    answering(feedOf())
    mount()
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockRejectedValue(
      new DOMException('blocked', 'NotAllowedError'),
    )
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    push(7)

    expect(
      await screen.findByRole('button', { name: 'Listen live' }),
    ).toBeInTheDocument()
  })

  /** An interrupted `play()` — a new source set before the last one started —
   *  is the browser's business, not a refusal. */
  it('shrugs off a play that was interrupted', async () => {
    answering(feedOf())
    mount()
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockRejectedValue(
      new DOMException('interrupted', 'AbortError'),
    )
    await userEvent.click(await screen.findByRole('button', { name: 'Listen live' }))
    await waitFor(() => expect(sent).toHaveLength(1))

    push(7)

    await screen.findByText('Fire 7')
    expect(screen.getByRole('button', { name: 'Stop' })).toBeInTheDocument()
  })
})

describe('playing one call', () => {
  it('plays the call the reader pressed, and stops it on a second press', async () => {
    answering(feedOf(call(2), call(1)))
    const root = mount()
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause')

    await userEvent.click(await screen.findByRole('button', { name: 'Play Fire 1' }))

    expect(audio(root).src).toBe(`${ORIGIN}/api/call/1/audio`)
    const stop = screen.getByRole('button', { name: 'Stop Fire 1' })
    expect(stop).toHaveAttribute('aria-pressed', 'true')

    await userEvent.click(stop)

    expect(pause).toHaveBeenCalled()
    expect(screen.getByRole('button', { name: 'Play Fire 1' })).toBeInTheDocument()
  })

  /** A row keeps its element across a re-render, so a keyboard reader's focus
   *  stays on the button they pressed. */
  it('keeps focus on the row the reader pressed', async () => {
    answering(feedOf(call(2), call(1)))
    mount()

    const button = await screen.findByRole('button', { name: 'Play Fire 1' })
    await userEvent.click(button)

    expect(document.activeElement).toBe(button)
  })

  it('offers no player for a call with no audio', async () => {
    answering(feedOf(call(1, { audioUrl: undefined, encrypted: true })))
    mount()

    const row = within(
      await screen.findByRole('list', { name: 'Recent calls' }),
    ).getByRole('listitem')
    expect(within(row).queryByRole('button')).toBeNull()
    expect(row).toHaveTextContent('Encrypted')
  })
})
