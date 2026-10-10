import { useEffect, useState } from 'react'

/**
 * The **display dimmer** (#77, spec US 57): whether the Live display has been
 * quiet for the `secs` the Operator chose, and so is drawn dim — rdio's
 * `dimmerDelay`, for the scanner left running on a wall or a dashboard.
 *
 * Lit while a Call is on the air; the quiet is counted from when it ends, and
 * from the last time the display was touched (`wake`), because a Listener
 * reaching for it is looking at it. `0` never dims, which is what ships.
 *
 * Counted in **stretches** rather than timestamps: each lighting and each touch
 * starts a new one, and the display is dim only when the timer of the stretch
 * it is in has run out — so a timer from a stretch that has already ended can
 * never darken the one after it.
 */
export function useDimmer(lit: boolean, secs: number): { dimmed: boolean; wake: () => void } {
  const [stretch, setStretch] = useState(0)
  const [darkened, setDarkened] = useState(-1)
  // A Call lighting the display ends the quiet it was in. Adjusted while
  // rendering, React's own pattern for state that follows a prop, so the frame
  // a Call arrives on is never drawn dim.
  const [wasLit, setWasLit] = useState(lit)
  if (lit !== wasLit) {
    setWasLit(lit)
    setStretch((count) => count + 1)
  }

  useEffect(() => {
    if (lit || secs <= 0) return
    const timer = setTimeout(() => setDarkened(stretch), secs * 1000)
    return () => clearTimeout(timer)
  }, [lit, secs, stretch])

  return {
    dimmed: !lit && secs > 0 && darkened === stretch,
    wake: () => setStretch((count) => count + 1),
  }
}
