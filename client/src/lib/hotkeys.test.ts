import { describe, expect, it } from 'vitest'

import { HOTKEYS, hotkeyFor, type KeyPress } from './hotkeys'

/** A key pressed with nothing else held, on the page itself. */
function press(key: string, over: Partial<KeyPress> = {}): KeyPress {
  return {
    key,
    shiftKey: false,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    repeat: false,
    target: document.body,
    ...over,
  }
}

function element(html: string): Element {
  const holder = document.createElement('div')
  holder.innerHTML = html
  return holder.firstElementChild!
}

/** The five controls a desktop Listener reaches for without looking (#78,
 *  spec US 55) — Hold being two, one for each thing it can hold. */
describe('hotkeyFor', () => {
  it.each([
    [' ', {}, 'pause'],
    ['k', {}, 'pause'],
    ['n', {}, 'skip'],
    ['ArrowRight', {}, 'skip'],
    ['r', {}, 'replay'],
    ['a', {}, 'avoid'],
    ['h', {}, 'holdTalkgroup'],
    ['H', { shiftKey: true }, 'holdSystem'],
    // Caps Lock is not a second keymap.
    ['N', {}, 'skip'],
  ] as const)('reads %o %o as %s', (key, over, command) => {
    expect(hotkeyFor(press(key, over))).toBe(command)
  })

  it.each([
    ['an unbound key', press('z')],
    ['a browser shortcut', press('r', { metaKey: true })],
    ['a Ctrl chord', press('n', { ctrlKey: true })],
    ['an Alt chord', press('a', { altKey: true })],
    // A held key fires keydown over and over; one press is one Skip.
    ['a key held down', press('n', { repeat: true })],
  ])('ignores %s', (_case, event) => {
    expect(hotkeyFor(event)).toBeNull()
  })

  /** Typing is typing: a filter box on Search must take an "a" as a letter. */
  it.each([
    '<input type="text">',
    '<textarea></textarea>',
    '<select><option>x</option></select>',
    '<div contenteditable="true"></div>',
  ])('leaves keys alone in %s', (html) => {
    expect(hotkeyFor(press('a', { target: element(html) }))).toBeNull()
    expect(hotkeyFor(press(' ', { target: element(html) }))).toBeNull()
  })

  /** Space *presses* a focused button — the keyboard user's click — and
   *  taking it would break the one key every control relies on. The letters
   *  are nobody's there, so they still work. */
  it.each(['<button>Skip</button>', '<a href="/search">Search</a>', '<div role="button"></div>'])(
    'gives Space to a focused %s, but not the letters',
    (html) => {
      expect(hotkeyFor(press(' ', { target: element(html) }))).toBeNull()
      expect(hotkeyFor(press('n', { target: element(html) }))).toBe('skip')
    },
  )

  /** A key pressed with nothing focused lands on the window itself. */
  it('reads a key whose target is no element at all', () => {
    expect(hotkeyFor(press(' ', { target: window }))).toBe('pause')
    expect(hotkeyFor(press('n', { target: null }))).toBe('skip')
  })

  /** The documented keymap and the reader are one table, so the two cannot
   *  disagree about what a key does. */
  it('documents every key it reads', () => {
    for (const { keys, command } of HOTKEYS) {
      for (const key of keys) {
        const shifted = key.startsWith('Shift+')
        const pressed = press(shifted ? key.slice(6) : key === 'Space' ? ' ' : key, {
          shiftKey: shifted,
        })
        expect(hotkeyFor(pressed)).toBe(command)
      }
    }
  })
})
