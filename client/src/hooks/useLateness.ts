import { useState } from 'react'

import { lateness } from '@/lib/archive'
import type { Call } from '@/types'

/** Which start of which Call a measurement belongs to. */
interface Heard {
  id: number
  playId: number
  at: number
}

/**
 * How late `call` was heard, measured **once, when it started playing** (#78).
 *
 * Lateness is a fact about a moment — this Call reached the speaker four
 * minutes after it was said — and measured against a ticking clock instead it
 * stopped being one: a Call heard on time turned "late" partway through a long
 * transmission, and the card a finished Call leaves behind counted up for as
 * long as the channel stayed quiet. `playId` is part of the key, because a
 * replay is a new start and the Call genuinely is older by then.
 *
 * Measured while rendering, React's pattern for state that follows a value, so
 * the frame a Call first appears on already says how late it is.
 */
export function useLateness(call: Call, playId: number): string | null {
  const [heard, setHeard] = useState<Heard>(() => ({ id: call.id, playId, at: Date.now() }))
  let at = heard.at
  if (call.id !== heard.id || playId !== heard.playId) {
    at = Date.now()
    setHeard({ id: call.id, playId, at })
  }
  return lateness(call, at)
}
