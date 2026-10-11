import { Pause, Play, SkipForward } from 'lucide-react'

import { CallFlags } from '@/components/CallFlags'
import { DockedBanner } from '@/components/layout/DockedBanner'
import { ChannelLed, StatusLamp } from '@/components/Led'
import { Button } from '@/components/ui/button'
import { useInstance } from '@/hooks/useInstance'
import { useNow } from '@/hooks/useNow'
import { lateness } from '@/lib/archive'
import { talkgroupName } from '@/lib/call'
import { ledForCall } from '@/lib/led'
import { shortcutsFor } from '@/lib/hotkeys'
import type { Strip } from '@/lib/strip'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  nextCall,
  selectIsBlocked,
  selectIsPaused,
  togglePause,
  wayBackAction,
} from '@/store/transport'

/**
 * The mini-player docked above the tab bar (#56, spec US 54).
 *
 * The Live screen is a whole player, so before this the app's state simply
 * vanished the moment a Listener went to Talkgroups or Search: no Call, no
 * controls, and — worse — no sign that the feed was switched off or that the
 * archive had the audio. rdio-scanner has the same hole and shows nothing at
 * all.
 *
 * It states no condition of its own. [`stripView`] decides both modes and the
 * words in them, and the shell decides whether there is a strip; this maps that
 * answer onto controls that already exist — `togglePause` and `nextCall` route
 * to whichever source owns the audio (`@/store/transport`), so the strip never
 * has to know which one that is.
 */
export function MiniPlayer({ strip }: { strip: Strip }) {
  return (
    // The same docked card the banners use, tightened: this row holds icon
    // buttons rather than a sentence. Named for the surface and not its
    // content, because the Search screen's own Run bar is already the region
    // called "Now playing" and this sits beside it there.
    <DockedBanner label="Mini player" className="px-3 py-2.5">
      {strip.mode === 'playing' ? (
        <Playing strip={strip} />
      ) : (
        <Quiet strip={strip} />
      )}
    </DockedBanner>
  )
}

/** What is playing, and the two controls a Listener reaches for without
 *  looking. Previous is deliberately absent: three icon buttons in a docked
 *  strip on a phone is a row of targets too small to hit, and the full set is
 *  one tap away on Live. */
function Playing({ strip }: { strip: Extract<Strip, { mode: 'playing' }> }) {
  const dispatch = useAppDispatch()
  const paused = useAppSelector(selectIsPaused)
  // The browser wants a tap before it will play (#78): the Live display's
  // question, asked on every other tab in the strip's own words.
  const blocked = useAppSelector(selectIsBlocked)
  const goesLive = useInstance().playbackGoesLive
  const { call } = strip
  // How late a *live* Call is being heard (#78) — the Live display's honesty,
  // carried to every other tab. Not for the archive, whose Calls are old by
  // definition and say where in the Run they are instead.
  const live = strip.source === 'live'
  const now = useNow(live, 15_000)
  const late = live ? lateness(call, now) : null
  const detail = late ? `${strip.detail} · ${late}` : strip.detail

  return (
    <>
      {/* docs/design/brief.md state 6: paused blinks, playing is steady — the
          same LED the Live screen draws, for the same Call. */}
      <ChannelLed color={ledForCall(call)} size={12} pulse={paused} />
      <div className="min-w-0 flex-1">
        <p className="flex items-center gap-1.5 truncate font-mono text-sm">
          <span className="truncate">{talkgroupName(call)}</span>
          {/* An emergency has to be legible without reading anything (#42),
              which means everywhere a Call is rendered and not only on Live. */}
          <CallFlags call={call} />
        </p>
        <p className="truncate font-mono text-[11px] text-muted-foreground">
          {blocked ? 'Your browser needs a tap first' : detail}
        </p>
      </div>
      <Button
        variant="outline"
        size="icon"
        aria-label={blocked ? 'Tap to listen' : paused ? 'Resume' : 'Pause'}
        aria-keyshortcuts={shortcutsFor('pause')}
        onClick={() => dispatch(togglePause())}
      >
        {paused ? (
          <Play className="size-4" aria-hidden />
        ) : (
          <Pause className="size-4" aria-hidden />
        )}
      </Button>
      <Button
        variant="outline"
        size="icon"
        aria-label="Skip"
        aria-keyshortcuts={shortcutsFor('skip')}
        onClick={() => dispatch(nextCall(goesLive))}
      >
        <SkipForward className="size-4" aria-hidden />
      </Button>
    </>
  )
}

/** Why nothing is playing, and the way back — the half a player has no way to
 *  show, and the reason this strip is more than a mini-player. */
function Quiet({ strip }: { strip: Extract<Strip, { mode: 'quiet' }> }) {
  const dispatch = useAppDispatch()

  return (
    <>
      <StatusLamp
        color={strip.badge.color}
        size={12}
        pulse={strip.badge.pulse}
        className="ml-1"
      />
      <p className="min-w-0 flex-1 truncate font-mono text-xs uppercase tracking-wider text-muted-foreground">
        {strip.badge.label}
      </p>
      <Button
        size="sm"
        variant="secondary"
        onClick={() => dispatch(wayBackAction[strip.does]())}
      >
        {strip.label}
      </Button>
    </>
  )
}
