import {
  Ban,
  Pause,
  Play,
  Power,
  Radio,
  RotateCcw,
  SkipForward,
} from 'lucide-react'
import type { ReactNode } from 'react'

import { CallFlags } from '@/components/CallFlags'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { Button } from '@/components/ui/button'
import { Waveform } from '@/components/Waveform'
import { callCategory, formatFrequency, systemName, talkgroupName } from '@/lib/call'
import { formatCallTime } from '@/lib/archive'
import { feedReadout, type FeedBadge, type FeedEmpty } from '@/lib/feed'
import { ledForCall } from '@/lib/led'
import { isSystemHold, isTalkgroupHold } from '@/lib/selection'
import { cn } from '@/lib/utils'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  advance,
  avoid,
  clearAvoids,
  replay,
  selectAvoidedCount,
  selectFeedStatus,
  selectHistory,
  selectHold,
  selectHasGap,
  selectLiveCall,
  selectLiveControls,
  selectLiveStatus,
  selectMissed,
  selectQueueDepth,
  toggleHoldSystem,
  toggleHoldTalkgroup,
  turnFeedOff,
  turnFeedOn,
} from '@/store/live'
import {
  previousCall,
  selectIsLiveSource,
  selectIsPaused,
  selectProgress,
  togglePause,
} from '@/store/transport'
import type { Call } from '@/types'

/** Spec US 14's timed avoids, in minutes. */
const AVOID_MINUTES = [30, 60, 120] as const

/** The dot and the word for it — both decided by [`feedReadout`], so this can
 *  only draw them. A state cannot end up amber but pulsing, or lit green under
 *  the words FEED OFF. */
function LinkState({ badge }: { badge: FeedBadge }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <StatusLed color={badge.color} size={8} pulse={badge.pulse} />
      {badge.label}
    </span>
  )
}

/**
 * Live scanner (home) — the hero screen (#11, spec US 9–17).
 *
 * The display reads out the Call playing now; the controls narrow, skip,
 * replay, avoid and pause it. All of that is *client* state (ADR-0004): the
 * server holds only the subscription matrix, which this screen changes by
 * holding and avoiding, so a narrowed feed stops arriving rather than arriving
 * and being thrown away.
 */
export function LiveScreen() {
  const dispatch = useAppDispatch()
  const call = useAppSelector(selectLiveCall)
  const status = useAppSelector(selectLiveStatus)
  const queued = useAppSelector(selectQueueDepth)
  const missed = useAppSelector(selectMissed)
  const gap = useAppSelector(selectHasGap)
  const history = useAppSelector(selectHistory)
  const hold = useAppSelector(selectHold)
  const avoiding = useAppSelector(selectAvoidedCount)
  const paused = useAppSelector(selectIsPaused)
  // The two derived layers (#88). Between them the screen states no condition of
  // its own: `feed` says why the feed is or is not delivering, and `can` says
  // what that leaves reachable — so a control cannot be gated on a rule the
  // header above it disagrees with.
  const feed = useAppSelector(selectFeedStatus)
  const can = useAppSelector(selectLiveControls)
  const readout = feedReadout(feed, status)
  // The switch's own position, which is a different question from why the feed
  // is quiet: in playback mode the feed is not delivering and the switch is
  // still on, so pressing it there is a request to switch it off.
  const off = feed === 'off'
  // The live feed's own progress — an archived Call interrupting it (US 26) is
  // on the element instead, and its position isn't this display's to draw.
  const progress = useAppSelector((state) =>
    selectIsLiveSource(state) ? selectProgress(state) : 0,
  )

  return (
    <Screen
      title="LIVE"
      status={
        <span className="inline-flex items-center gap-3">
          <span
            role="status"
            aria-label="Queued calls"
            className="font-mono text-[11px] tabular-nums text-muted-foreground"
          >
            Q {queued}
          </span>
          <LinkState badge={readout.badge} />
        </span>
      }
    >
      {call ? (
        <Display
          call={call}
          progress={progress}
          paused={paused}
          missed={missed}
          gap={gap}
        />
      ) : (
        <Idle empty={readout.empty} missed={missed} gap={gap} />
      )}

      {/* The master switch (#80). First, and its own row, because it governs
          everything below it — and because a listener reaching for silence
          should not have to find it among the per-Call controls. */}
      <div className="mt-4">
        <Control
          label="Live feed"
          pressed={!off}
          onClick={() => dispatch(off ? turnFeedOn() : turnFeedOff())}
          icon={
            off ? (
              <Power className="size-3.5" aria-hidden />
            ) : (
              <Radio className="size-3.5" aria-hidden />
            )
          }
        >
          {off ? 'Feed off — turn on' : 'Live feed on'}
        </Control>
      </div>

      <div className="mt-2 grid grid-cols-3 gap-2">
        <Control
          label="Hold system"
          pressed={isSystemHold(hold)}
          disabled={!can.holdSystem}
          onClick={() => dispatch(toggleHoldSystem())}
        >
          Hold sys
        </Control>
        <Control
          label="Hold talkgroup"
          pressed={isTalkgroupHold(hold)}
          disabled={!can.holdTalkgroup}
          onClick={() => dispatch(toggleHoldTalkgroup())}
        >
          Hold TG
        </Control>
        <Control
          label="Skip"
          disabled={!can.skip}
          onClick={() => dispatch(advance())}
          icon={<SkipForward className="size-3.5" aria-hidden />}
        >
          Skip
        </Control>

        <Control
          label="Replay"
          disabled={!can.replay}
          // US 13 is "current, previous, and back through the last five": the
          // Call playing starts over, and with none playing the last one
          // returns. The list below reaches further back.
          onClick={() => dispatch(call ? replay(call.id) : previousCall())}
          icon={<RotateCcw className="size-3.5" aria-hidden />}
        >
          Replay
        </Control>
        <Control
          label={paused ? 'Resume' : 'Pause'}
          disabled={!can.pause}
          onClick={() => dispatch(togglePause())}
          icon={
            paused ? (
              <Play className="size-3.5" aria-hidden />
            ) : (
              <Pause className="size-3.5" aria-hidden />
            )
          }
        >
          {paused ? 'Resume' : 'Pause'}
        </Control>
        <Control
          label="Avoid"
          disabled={!can.avoid}
          onClick={() => dispatch(avoid({ until: 0 }))}
          icon={<Ban className="size-3.5" aria-hidden />}
        >
          Avoid
        </Control>
      </div>

      {/* The timed cycle (spec US 14): a chatty Talkgroup goes quiet for a
          spell and comes back on its own — one tap each, rather than rdio's
          hunt through a menu. */}
      <div className="mt-2 grid grid-cols-3 gap-2">
        {AVOID_MINUTES.map((minutes) => (
          <Control
            key={minutes}
            label={`Avoid for ${minutes} minutes`}
            disabled={!can.avoid}
            onClick={() => dispatch(avoid({ until: Date.now() + minutes * 60_000 }))}
          >
            {minutes} min
          </Control>
        ))}
      </div>

      {avoiding > 0 && (
        <div className="mt-2">
          <Control
            label={`Stop avoiding ${avoiding} talkgroups`}
            onClick={() => dispatch(clearAvoids())}
          >
            Avoiding {avoiding} — clear
          </Control>
        </div>
      )}

      <History
        calls={history}
        disabled={!can.recent}
        onReplay={(id) => dispatch(replay(id))}
      />
    </Screen>
  )
}

/** The scanner readout: who is talking, on what, and where in the Call we are. */
function Display({
  call,
  progress,
  paused,
  missed,
  gap,
}: {
  call: Call
  progress: number
  paused: boolean
  missed: number
  gap: boolean
}) {
  const color = ledForCall(call)

  return (
    <section
      aria-label="Scanner display"
      className="rounded-xl border border-border bg-card px-4 py-4"
    >
      <div className="flex items-start gap-3">
        {/* docs/design/brief.md state 6: paused blinks, playing is steady. */}
        <StatusLed color={color} size={16} pulse={paused} className="mt-1.5" />
        <div className="min-w-0 flex-1">
          <p className="flex items-center gap-2 truncate font-mono text-lg leading-tight">
            <span className="truncate">{talkgroupName(call)}</span>
            {/* An emergency has to be legible without reading anything (#42,
                spec US 5) — it is the flag #53 will also push on. */}
            <CallFlags call={call} />
          </p>
          <p className="truncate font-mono text-xs text-muted-foreground">
            {systemName(call)}
          </p>
          {callCategory(call) && (
            <p className="truncate font-mono text-[11px] text-muted-foreground/70">
              {callCategory(call)}
            </p>
          )}
        </div>
        {paused && (
          <span className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
            Paused
          </span>
        )}
      </div>

      <Waveform
        seed={call.id}
        color={color}
        progress={progress}
        live={!paused}
        className="mt-4"
      />

      <dl className="mt-4 grid grid-cols-2 gap-x-4 gap-y-2 font-mono text-[11px]">
        <Stat name="Frequency" testId="stat-frequency">
          {formatFrequency(call.frequency)}
        </Stat>
        <Stat name="TGID" testId="stat-talkgroup">
          {call.talkgroupRef}
        </Stat>
        <Stat name="Unit ID" testId="stat-unit">
          {call.source ?? '—'}
        </Stat>
        <Stat name="Time" testId="stat-time">
          {formatCallTime(call.timestamp)}
        </Stat>
      </dl>

      <Missed count={missed} gap={gap} />
    </section>
  )
}

/**
 * Zero-config first run, and every lull after it — saying *which* lull this is
 * (#88).
 *
 * The words come from the same [`feedReadout`] the header's dot does, because a
 * silence someone chose looks exactly like a fault they didn't, and a listener
 * can only act on one of them. Two readers of one answer, so they cannot
 * describe two different situations.
 */
function Idle({
  empty,
  missed,
  gap,
}: {
  empty: FeedEmpty
  missed: number
  gap: boolean
}) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-xl border border-border bg-card px-6 py-12 text-center">
      <Radio className="size-6 text-muted-foreground" aria-hidden />
      <p className="font-mono text-sm text-muted-foreground">{empty.headline}</p>
      <p className="max-w-xs text-xs text-muted-foreground/70">{empty.detail}</p>
      <Missed count={missed} gap={gap} />
    </div>
  )
}

/**
 * Calls the listener will not hear, said out loud — rdio drops them silently
 * (ADR-0004's `lagged` and `gap`).
 *
 * Two ways to miss traffic, and the difference is whether anyone can count it.
 * A lagging connection drops a known number; a **Backfill** that could not reach
 * back far enough leaves a hole the server cannot size without walking the whole
 * archive. So a gap says "some", and a gap *on top of* a count says the count is
 * a floor — because "3 missed" when it was really thirty is worse than admitting
 * the number is not the whole story.
 */
function Missed({ count, gap }: { count: number; gap: boolean }) {
  if (count === 0 && !gap) return null
  const how = count > 0 ? `${count}${gap ? '+' : ''}` : 'some'
  return (
    <p className="mt-3 font-mono text-[11px] text-led-red/80" role="status">
      {how} missed — search the archive to catch up
    </p>
  )
}

function Stat({
  name,
  testId,
  children,
}: {
  name: string
  testId: string
  children: ReactNode
}) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="text-[10px] uppercase tracking-wider text-muted-foreground">
        {name}
      </dt>
      <dd data-testid={testId} className="tabular-nums">
        {children}
      </dd>
    </div>
  )
}

/** One control. `label` is what it's called; the children are what fits. */
function Control({
  label,
  pressed,
  disabled,
  onClick,
  icon,
  children,
}: {
  label: string
  pressed?: boolean
  disabled?: boolean
  onClick: () => void
  icon?: ReactNode
  children: ReactNode
}) {
  return (
    <Button
      variant="outline"
      size="sm"
      aria-label={label}
      aria-pressed={pressed}
      disabled={disabled}
      onClick={onClick}
      className={cn(
        'gap-1.5 font-mono text-[11px] uppercase tracking-wider',
        pressed && 'border-foreground text-foreground',
      )}
    >
      {icon}
      {children}
    </Button>
  )
}

/** The last few Calls, newest first — tap one to hear it again (spec US 13). */
function History({
  calls,
  onReplay,
  disabled,
}: {
  calls: Call[]
  onReplay: (id: number) => void
  /** The live feed is not the listener's audio (#88), so replay does nothing —
   *  say so in the rows rather than letting them look clickable. Switching off
   *  files the Call it cut short into history, so this list is guaranteed to
   *  have something in it right when the feed goes off: a row that silently did
   *  nothing would be the common case, not a corner. */
  disabled: boolean
}) {
  if (calls.length === 0) return null

  return (
    <section className="mt-6">
      <h2 className="mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground">
        Recent
      </h2>
      <ul
        aria-label="Recent calls"
        className="divide-y divide-border rounded-xl border border-border bg-card"
      >
        {calls.map((call) => (
          <li key={`${call.id}`}>
            <button
              type="button"
              disabled={disabled}
              onClick={() => onReplay(call.id)}
              className="flex w-full items-center gap-3 px-3 py-2.5 text-left transition-colors hover:bg-muted/40 disabled:opacity-40"
            >
              <StatusLed
                color={ledForCall(call)}
                size={10}
              />
              <span className="flex min-w-0 flex-1 items-center gap-1.5 truncate font-mono text-sm">
                <span className="truncate">{talkgroupName(call)}</span>
                {/* Encrypted Calls only ever appear here — they never play
                    (#42, spec US 9) — so this is the one place their badge is
                    the whole of what says the channel was busy. */}
                <CallFlags call={call} />
              </span>
              <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                {formatCallTime(call.timestamp)}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  )
}
