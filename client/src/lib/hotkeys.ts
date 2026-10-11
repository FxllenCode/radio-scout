/**
 * The keyboard, for a desktop Listener (#78, spec US 55).
 *
 * Five controls a Listener reaches for without looking — pause, skip, replay,
 * avoid and hold, Hold being two — on keys that work on every tab, because the
 * audio follows the Listener between tabs (#56) and a key that only worked on
 * Live would be a key that did nothing half the time. rdio has none: it listens
 * to the keyboard only to know somebody is there.
 *
 * Pure, over the facts of one key press, so what a key means is a table rather
 * than a handler's branches. What a command then *does*, and whether it is
 * allowed, is the shell's (`hooks/useHotkeys`), which asks the same
 * `controlsFor` the buttons do — a key cannot reach a control the screen shows
 * as dead.
 *
 * **Stream Deck and media keys need nothing here.** The Media Session already
 * answers play/pause and next from the keyboard's media keys, a headset and an
 * Elgato Multimedia action, with the window unfocused — which is the half of
 * desktop listening a page's own keys cannot do. `docs/using.md` says so.
 */

/** What a key can ask for. */
export type HotkeyCommand =
  | 'pause'
  | 'skip'
  | 'replay'
  | 'avoid'
  | 'holdTalkgroup'
  | 'holdSystem'

/**
 * The keymap, in the spelling `aria-keyshortcuts` and a help page use —
 * `Space` for the bar, `Shift+` for a chord.
 *
 * The reader below is built from this table, and a test reads every row back
 * through it, so the documented keys and the handled keys are one list.
 */
export const HOTKEYS: readonly { command: HotkeyCommand; keys: readonly string[] }[] = [
  { command: 'pause', keys: ['Space', 'K'] },
  { command: 'skip', keys: ['N', 'ArrowRight'] },
  { command: 'replay', keys: ['R'] },
  { command: 'avoid', keys: ['A'] },
  { command: 'holdTalkgroup', keys: ['H'] },
  { command: 'holdSystem', keys: ['Shift+H'] },
]

/** A command's keys, as `aria-keyshortcuts` wants them: alternatives separated
 *  by spaces. */
export const shortcutsFor = (command: HotkeyCommand): string =>
  HOTKEYS.find((one) => one.command === command)!.keys.join(' ')

/** The facts of one key press that decide what it means. */
export interface KeyPress {
  key: string
  shiftKey: boolean
  ctrlKey: boolean
  metaKey: boolean
  altKey: boolean
  repeat: boolean
  target: EventTarget | null
}

/** A key, as the table spells it: a letter upper-cased (Caps Lock is not a
 *  second keymap), the bar as `Space`, anything else as the browser names it. */
function spelled({ key, shiftKey }: KeyPress): string {
  const name = key === ' ' ? 'Space' : key.length === 1 ? key.toUpperCase() : key
  return shiftKey ? `Shift+${name}` : name
}

const BY_KEY = new Map(
  HOTKEYS.flatMap(({ command, keys }) => keys.map((key) => [key, command] as const)),
)

/** Where typing is typing. */
const EDITABLE = 'input, textarea, select, [contenteditable=""], [contenteditable="true"]'

/** What a focused Space already means: press this. */
const PRESSABLE = 'button, a[href], summary, [role="button"], [role="link"], [role="switch"], [role="checkbox"]'

/**
 * The command a key press asks for, or `null` for one that is somebody
 * else's.
 *
 * Somebody else's: a chord with Ctrl, Cmd or Alt (the browser's and the
 * OS's), a key held down (one press is one Skip, not twelve), anything typed
 * into a field — and Space on a focused button or link, which *presses* it:
 * the keyboard user's click, and the one key every control relies on.
 */
export function hotkeyFor(press: KeyPress): HotkeyCommand | null {
  if (press.ctrlKey || press.metaKey || press.altKey || press.repeat) return null
  const target = press.target instanceof Element ? press.target : null
  if (target?.closest(EDITABLE)) return null
  if (press.key === ' ' && target?.closest(PRESSABLE)) return null
  return BY_KEY.get(spelled(press)) ?? null
}
