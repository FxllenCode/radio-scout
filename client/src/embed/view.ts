/**
 * The **Embed** page's DOM (#75): [`PlayerState`] drawn, and the reader's
 * presses handed back.
 *
 * Plain DOM rather than React, because this page is loaded by every reader of
 * somebody else's homepage and the app's bundle is half a megabyte; what is
 * drawn here is a heading, a button and a list. Two rules keep it honest:
 *
 * - **Text is only ever `textContent`.** A talkgroup label is a string a
 *   Recorder sent, and this page runs on an Instance's origin inside a
 *   stranger's frame — markup built from it would be the one place on the
 *   Instance a label could become script.
 * - **A row keeps its element** for as long as its Call is listed, and is only
 *   moved, so a keyboard reader's focus stays on the button they pressed while
 *   live Calls arrive above it.
 */
import { formatCallTime, formatDuration } from '@/lib/archive'
import { systemName, talkgroupName } from '@/lib/call'
import { LED_HEX, ledForCall } from '@/lib/led'
import { encodeSelection } from '@/lib/selectionUrl'
import type { Call } from '@/types'

import type { PlayerState } from './player'

export interface ViewHandlers {
  onListen(): void
  onStop(): void
  onPlay(id: number): void
}

export interface View {
  render(state: PlayerState): void
}

const SVG = 'http://www.w3.org/2000/svg'

/** A glyph as an inline SVG — not `▶`, which iOS draws as a colour emoji. */
function icon(path: string): SVGSVGElement {
  const svg = document.createElementNS(SVG, 'svg')
  svg.setAttribute('viewBox', '0 0 16 16')
  svg.setAttribute('aria-hidden', 'true')
  const shape = document.createElementNS(SVG, 'path')
  shape.setAttribute('d', path)
  svg.append(shape)
  return svg
}

const PLAY = 'M4 2.5v11l9-5.5z'
const STOP = 'M3.5 3.5h9v9h-9z'

function element<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const made = document.createElement(tag)
  made.className = className
  if (text !== undefined) made.textContent = text
  return made
}

/** When a Call happened, as a reader of a homepage reads it: the time of day
 *  for today, the date as well for anything older. */
export function when(ms: number | undefined, now: number = Date.now()): string {
  if (ms === undefined) return ''
  // The app's own fixed `YYYY-MM-DD HH:MM:SS` — less its date, for today.
  const full = formatCallTime(ms)
  return new Date(ms).toDateString() === new Date(now).toDateString()
    ? full.slice('YYYY-MM-DD '.length)
    : full
}

/** What the status line says: the socket while listening, else a Call that
 *  would not play, else nothing. */
function statusText(state: PlayerState): string {
  if (!state.listening) {
    return state.failed === undefined
      ? ''
      : "Couldn't play that call. The scanner may be unreachable."
  }
  switch (state.live) {
    case 'connected':
      return 'Live'
    case 'connecting':
      return 'Connecting…'
    case 'offline':
      return 'Reconnecting…'
  }
}

/** The sentence a page says instead of a list, if it says one. */
function note(state: PlayerState): string | undefined {
  switch (state.phase) {
    case 'loading':
      return 'Loading…'
    case 'unavailable':
      return 'This feed is no longer available.'
    case 'unreachable':
      return "Can't reach the scanner right now. Trying again…"
    case 'ready':
      return state.calls.length === 0 ? 'No calls yet.' : undefined
  }
}

export function createView(
  root: HTMLElement,
  audio: HTMLAudioElement,
  handlers: ViewHandlers,
): View {
  const frame = element('div', 'embed')
  const bar = element('header', 'bar')
  const heading = element('h1', 'name', 'Radio-Scout')
  const listen = element('button', 'listen')
  listen.type = 'button'
  listen.addEventListener('click', () =>
    listen.dataset.listening === 'true' ? handlers.onStop() : handlers.onListen(),
  )
  bar.append(heading, listen)

  const status = element('p', 'status')
  status.setAttribute('role', 'status')
  const message = element('p', 'note')
  const list = element('ul', 'calls')
  list.setAttribute('aria-label', 'Recent calls')
  const foot = element('footer', 'foot')
  const open = element('a', 'open', 'Open in Radio-Scout')
  open.target = '_blank'
  open.rel = 'noopener'
  foot.append(open)

  frame.append(bar, status, message, list, foot, audio)
  root.replaceChildren(frame)

  const rows = new Map<number, Row>()

  return {
    render(state) {
      if (state.name !== undefined) heading.textContent = state.name
      document.title = state.name ?? 'Radio-Scout'

      const ready = state.phase === 'ready'
      listen.hidden = !ready
      listen.dataset.listening = String(state.listening)
      listen.replaceChildren(
        icon(state.listening ? STOP : PLAY),
        document.createTextNode(state.listening ? 'Stop' : 'Listen live'),
      )
      frame.dataset.live = state.listening ? state.live : 'off'
      status.textContent = statusText(state)

      const said = note(state)
      message.hidden = said === undefined
      message.textContent = said ?? ''

      foot.hidden = !ready || state.selection === undefined
      if (state.selection) {
        open.href = `/talkgroups?sel=${encodeSelection(state.selection)}`
      }

      // Rows: drop what left, make what arrived, keep the rest — and put each
      // in its place, moving an element rather than rebuilding it.
      const listed = new Set(state.calls.map((it) => it.id))
      for (const [id, row] of rows) {
        if (!listed.has(id)) {
          row.item.remove()
          rows.delete(id)
        }
      }
      state.calls.forEach((call, index) => {
        let row = rows.get(call.id)
        if (!row) {
          row = makeRow(call, handlers)
          rows.set(call.id, row)
        }
        row.update(call.id === state.playing)
        if (list.children[index] !== row.item) {
          list.insertBefore(row.item, list.children[index] ?? null)
        }
      })
    },
  }
}

interface Row {
  item: HTMLLIElement
  update(playing: boolean): void
}

function makeRow(call: Call, handlers: ViewHandlers): Row {
  const item = element('li', 'call')
  const led = element('span', 'led')
  led.setAttribute('aria-hidden', 'true')
  led.style.background = LED_HEX[ledForCall(call)]

  const what = element('div', 'what')
  const facts = [systemName(call), when(call.timestamp)]
  if (call.durationMs !== undefined) facts.push(formatDuration(call.durationMs))
  if (call.encrypted) facts.push('Encrypted')
  what.append(
    element('span', 'talkgroup', talkgroupName(call)),
    element('span', 'facts', facts.filter(Boolean).join(' · ')),
  )
  item.append(led, what)

  // An Encrypted Call is the activity and no audio (spec US 9): facts, and no
  // button that would play nothing.
  if (call.audioUrl === undefined) return { item, update() {} }

  const button = element('button', 'play')
  button.type = 'button'
  button.addEventListener('click', () => handlers.onPlay(call.id))
  item.append(button)

  return {
    item,
    update(playing) {
      item.dataset.playing = String(playing)
      button.setAttribute('aria-pressed', String(playing))
      button.setAttribute(
        'aria-label',
        `${playing ? 'Stop' : 'Play'} ${talkgroupName(call)}`,
      )
      button.replaceChildren(icon(playing ? STOP : PLAY))
    },
  }
}
