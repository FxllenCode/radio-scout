/**
 * The star glyph, filled or hollow (#66, spec US 37).
 *
 * Its own component because three surfaces draw it and only one of them is a
 * [`StarButton`]: the Live display's **Control** and the session log's action
 * sheet each own their own frame, and a hand-copied `fill-current` is how a
 * Star comes to look kept on one screen and not on another. What is shared is
 * exactly this — the glyph and what filling it means — where the *label* is
 * each surface's own (`useStar`'s `verb`).
 */
import { Star } from 'lucide-react'

import { cn } from '@/lib/utils'

export function StarMark({ starred }: { starred: boolean }) {
  return (
    <Star
      className={cn('size-4', starred && 'fill-current text-amber-400')}
      aria-hidden
    />
  )
}
