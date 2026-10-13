import {
  Ban,
  FastForward,
  ListOrdered,
  Maximize,
  Minimize,
  Pause,
  Play,
  Power,
  Radio,
  RotateCcw,
  SkipForward,
  Users,
  Zap,
} from 'lucide-react'
import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router-dom'

import { AvoidSheet } from '@/components/AvoidSheet'
import { CallFlags } from '@/components/CallFlags'
import { CallTime } from '@/components/CallTime'
import { Screen } from '@/components/layout/Screen'
import { QueueSheet } from '@/components/QueueSheet'
import { StarButton } from '@/components/StarButton'
import { StarMark } from '@/components/StarMark'
import { ChannelLed, StatusLamp } from '@/components/Led'
import { UnitLink } from '@/components/UnitLink'
import { useStar } from '@/hooks/useStar'
import { useClock, useInstance } from '@/hooks/useInstance'
import { useDimmer } from '@/hooks/useDimmer'
import { useLateness } from '@/hooks/useLateness'
import { useReadoutFacts } from '@/hooks/useReadoutFacts'
import { useFullscreen } from '@/hooks/useFullscreen'
import { DATE_TICK_MS, useNow } from '@/hooks/useNow'
import { Button } from '@/components/ui/button'
import { Waveform } from '@/components/Waveform'
import { callCategory, formatFrequency, systemName, talkgroupName, unitName } from '@/lib/call'
import { formatClock } from '@/lib/archive'
import { CATCHUP_RATE } from '@/lib/catchup'
import { feedReadout, type FeedBadge, type FeedEmpty } from '@/lib/feed'
import { ledForCall } from '@/lib/led'
import { DEFAULT_SEARCH_URL, writeSearchUrl } from '@/lib/searchUrl'
import { shortcutsFor, type HotkeyCommand } from '@/lib/hotkeys'
import { isSystemHold, isTalkgroupHold } from '@/lib/selection'
import { wayBack } from '@/lib/strip'
import { cn } from '@/lib/utils'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  advance,
  avoid,
  replay,
  selectAvoidedCount,
  selectDisplay,
  selectFeedStatus,
  selectHistory,
  selectHold,
  selectHasGap,
  selectListenerCount,
  selectLiveControls,
  selectLiveStatus,
  selectIsPriority,
  selectMissed,
  selectMissedFrom,
  selectPlayId,
  missedCleared,
  selectIsCatchingUp,
  selectQueueDepth,
  toggleHoldSystem,
  toggleHoldTalkgroup,
  togglePriorityShown,
  turnFeedOff,
  turnFeedOn,
} from '@/store/live'
import {
  replayLive,
  selectIsBlocked,
  selectIsLiveSource,
  selectIsPaused,
  selectProgress,
  togglePause,
  wayBackAction,
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
      <StatusLamp color={badge.color} size={8} pulse={badge.pulse} />
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
export function LiveScreen({ docked = false }: { docked?: boolean } = {}) {
  const dispatch = useAppDispatch()
  const status = useAppSelector(selectLiveStatus)
  const queued = useAppSelector(selectQueueDepth)
  const catchingUp = useAppSelector(selectIsCatchingUp)
  const missed = useAppSelector(selectMissed)
  const gap = useAppSelector(selectHasGap)
  const history = useAppSelector(selectHistory)
  const hold = useAppSelector(selectHold)
  const avoiding = useAppSelector(selectAvoidedCount)
  /** Which sheet is open, if either (#58). One piece of state rather than two
   *  booleans, so the two can never both be up. */
  const [sheet, setSheet] = useState<'queue' | 'avoids' | null>(null)
  const paused = useAppSelector(selectIsPaused)
  const blocked = useAppSelector(selectIsBlocked)
  // The two derived layers (#88). Between them the screen states no condition of
  // its own: `feed` says why the feed is or is not delivering, and `can` says
  // what that leaves reachable — so a control cannot be gated on a rule the
  // header above it disagrees with.
  const feed = useAppSelector(selectFeedStatus)
  const can = useAppSelector(selectLiveControls)
  // Why the server is turning us away (#77), whose fault a dead link is, and
  // whether there is anything to listen *to* yet (#78) — the same facts the
  // announcer says, gathered once.
  const readout = feedReadout(feed, status, useReadoutFacts())
  // How many are listening, when the Operator publishes it (#77) — and only
  // while live, because a count kept from a dropped socket is a number about a
  // moment that has passed.
  const listeners = useAppSelector(selectListenerCount)
  const counted = feed === 'live' ? listeners : null
  // The switch's own position, which is a different question from why the feed
  // is quiet: in playback mode the feed is not delivering and the switch is
  // still on, so pressing it there is a request to switch it off.
  const off = feed === 'off'
  // The one tap out of whichever silence this is, named once for the whole app.
  const exit = wayBack(feed)
  /**
   * The Call the display is showing, which outlives the transmission (#56).
   *
   * A scanner's readout does not blank the instant a Talkgroup unkeys, and this
   * one used to: the last Call went to RECENT and the screen fell back to
   * "waiting for the first call", which is a lie about an archive with Calls in
   * it — and it left *Hold* and *Avoid* pointing at nothing at exactly the
   * moment a Listener reaches for them.
   *
   * Read from `@/store/live` rather than assembled here, because the reducers
   * behind those controls resolve the very same Call: a second spelling of the
   * precedence is how a lit button comes to act on something the card is not
   * showing. The two silences a Listener *chose* are folded into that answer
   * too, and keep #88's words instead — a card with every control dead says
   * strictly less than the sentence explaining how to get the feed back.
   */
  const { call: showing, ended } = useAppSelector(selectDisplay)
  const dimmer = useDimmer(showing !== null && !ended, useInstance().dimmerSecs)
  const fullscreen = useFullscreen()
  // Asked of the Call the display is showing, so the control reads as pressed
  // for exactly the Talkgroup pressing it would act on (#58).
  const prioritized = useAppSelector((state) =>
    showing ? selectIsPriority(state, showing.systemRef, showing.talkgroupRef) : false,
  )
  // The **Star** on that same Call (#66, spec US 37) — one rule shared with
  // the row control, so the display and a search row cannot come to disagree
  // about what a Star is or what tapping it does.
  const { starred, verb: starVerb, toggle: toggleStar } = useStar(showing)
  // The live feed's own progress — an archived Call interrupting it (US 26) is
  // on the element instead, and its position isn't this display's to draw.
  const progress = useAppSelector((state) =>
    selectIsLiveSource(state) ? selectProgress(state) : 0,
  )

  return (
    <Screen
      title="LIVE"
      // Beside another screen on a desk (#78), where it is the second heading
      // on the page rather than the first.
      heading={docked ? 2 : 1}
      status={
        <span className="inline-flex items-center gap-3">
          {/* The `Q` count was a number for four tickets, and this is where it
              becomes a tool (#58, spec US 24): the same readout, now the way in
              to what is behind it.

              Reachable at zero, because US 24 asks to *see* what is waiting and
              "nothing is waiting" is an answer — the sheet says so. What it is
              gated on is the feed (`can.queue`): **Feed off** and **Playback
              mode** empty the queue by construction, so offering it there would
              be a control that looks live and does nothing, which is what #88's
              own tests exist to catch.

              The live region is the wrapper, not the button, so a screen reader
              is still told the depth changed without being told a button
              appeared. */}
          <span role="status" className="inline-flex">
            <button
              type="button"
              aria-label={`Queued calls: ${queued}`}
              disabled={!can.queue}
              onClick={() => setSheet('queue')}
              className="-my-3 inline-flex min-h-11 min-w-11 items-center justify-center gap-1 font-mono text-[11px] tabular-nums text-muted-foreground transition-colors hover:text-foreground disabled:hover:text-muted-foreground"
            >
              <ListOrdered className="size-3" aria-hidden />
              Q {queued}
            </button>
          </span>
          {/* **The screen never lies about the feed** (#56), applied to the one
              thing #59 adds that a Listener cannot otherwise see: audio is
              playing at one and a half times and skipping the quiet, and
              nothing else on this screen would say so. Rendered beside the `Q`
              rather than inside the button because it is a readout and that is
              a control — and only while it is on, so a Listener who is not
              catching up reads exactly what they read before. */}
          {catchingUp && (
            <span
              role="status"
              className="inline-flex items-center gap-1 font-mono text-[11px] tabular-nums text-led-amber"
            >
              <FastForward className="size-3" aria-hidden />
              {CATCHUP_RATE}×
            </span>
          )}
          {counted !== null && (
            <span
              aria-label={`${counted} listening`}
              className="inline-flex items-center gap-1 font-mono text-[11px] tabular-nums text-muted-foreground"
            >
              <Users className="size-3" aria-hidden />
              {counted}
            </span>
          )}
          <Clock />
          {fullscreen.available && (
            <button
              type="button"
              aria-label={fullscreen.active ? 'Exit full screen' : 'Full screen'}
              onClick={fullscreen.toggle}
              className="-my-3 inline-flex size-11 items-center justify-center rounded-md text-muted-foreground transition-colors hover:text-foreground"
            >
              {fullscreen.active ? (
                <Minimize className="size-4" aria-hidden />
              ) : (
                <Maximize className="size-4" aria-hidden />
              )}
            </button>
          )}
          <LinkState badge={readout.badge} />
        </span>
      }
    >
      {/* The Operator's **display dimmer** (#77): the readout dims after a
          quiet they chose and lights for the next Call — or a touch, because
          somebody reaching for it is looking at it. Only the readout: the
          controls under it stay as bright as a control has to be to be used. */}
      <div
        data-testid="scanner-readout"
        data-dimmed={dimmer.dimmed}
        onPointerDown={dimmer.wake}
        // rdio's gesture, kept (#78): a double-tap on the readout. Only where
        // the browser can — the button above is the discoverable half.
        onDoubleClick={fullscreen.available ? fullscreen.toggle : undefined}
        className={cn('transition-opacity duration-700', dimmer.dimmed && 'opacity-25')}
      >
        {showing ? (
          <Display
            call={showing}
            ended={ended}
            progress={progress}
            paused={paused}
            blocked={blocked}
            onListen={() => dispatch(togglePause())}
            missed={missed}
            gap={gap}
          />
        ) : (
          <Idle empty={readout.empty} missed={missed} gap={gap} />
        )}
      </div>

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

      {/* The way out of **Playback mode** (#56, spec US 54), and deliberately
          *not* the switch above it: that switch reads on here and is right to —
          the feed was never switched off, playback borrowed the audio — so
          overloading it would trade one lie for another (#88). Its own row,
          because with every control below it dead this is the only thing on the
          screen worth pressing.
          Only playback: **Feed off**'s way back is the switch itself, and
          offering it twice would be two buttons for one action. The word and
          the action are the mini-player's own ([`wayBack`]), so the two
          surfaces cannot come to call it different things. */}
      {feed === 'playback' && exit && (
        <div className="mt-2">
          <Control
            label={exit.label}
            onClick={() => dispatch(wayBackAction[exit.does]())}
            icon={<Radio className="size-3.5" aria-hidden />}
          >
            {exit.label}
          </Control>
        </div>
      )}

      <div className="mt-2 grid grid-cols-3 gap-2">
        <Control
          label="Hold system"
          hotkey="holdSystem"
          pressed={isSystemHold(hold)}
          disabled={!can.holdSystem}
          onClick={() => dispatch(toggleHoldSystem())}
        >
          Hold sys
        </Control>
        <Control
          label="Hold talkgroup"
          hotkey="holdTalkgroup"
          pressed={isTalkgroupHold(hold)}
          disabled={!can.holdTalkgroup}
          onClick={() => dispatch(toggleHoldTalkgroup())}
        >
          Hold TG
        </Control>
        <Control
          label="Skip"
          hotkey="skip"
          disabled={!can.skip}
          onClick={() => dispatch(advance())}
          icon={<SkipForward className="size-3.5" aria-hidden />}
        >
          Skip
        </Control>

        <Control
          label="Replay"
          hotkey="replay"
          disabled={!can.replay}
          // US 13 is "current, previous, and back through the last five" —
          // `replayLive`, which the R key shares. The list below reaches
          // further back.
          onClick={() => dispatch(replayLive())}
          icon={<RotateCcw className="size-3.5" aria-hidden />}
        >
          Replay
        </Control>
        <Control
          label={paused ? 'Resume' : 'Pause'}
          hotkey="pause"
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
          hotkey="avoid"
          disabled={!can.avoid}
          onClick={() => dispatch(avoid({ until: 0 }))}
          icon={<Ban className="size-3.5" aria-hidden />}
        >
          Avoid
        </Control>
      </div>

      {/* **Priority** on the Talkgroup being shown (#58, spec US 27) — the
          moment a Listener realises dispatch should outrank tactical chatter is
          while they are hearing it, and the Talkgroups panel is two taps and a
          four-hundred-row list away. Its own row rather than a seventh cell,
          because it is the only control here that changes what plays *later*
          rather than now — and since #66 it shares that row with the other one
          that is not about *now*: keeping the Call after it has gone.

          Both act on the Call the display is showing (#56's subject), so a
          Listener who reaches for either while hearing something is marking the
          thing they heard. */}
      <div className="mt-2 grid grid-cols-2 gap-2">
        <Control
          label={
            prioritized ? 'Clear priority on this talkgroup' : 'Give this talkgroup priority'
          }
          pressed={prioritized}
          disabled={!can.priority}
          onClick={() => dispatch(togglePriorityShown())}
          icon={<Zap className="size-3.5" aria-hidden />}
        >
          {prioritized ? 'Priority on' : 'Priority'}
        </Control>
        <Control
          label={`${starVerb} this call`}
          pressed={starred}
          disabled={!showing}
          onClick={toggleStar}
          icon={<StarMark starred={starred} />}
        >
          {/* The *state* as the body and the *action* in the label, which is
              what every Control in this grid does — Priority reads "Priority
              on" over "Clear priority on this talkgroup". */}
          {starred ? 'Starred' : 'Star'}
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

      {/* Opens the list rather than clearing the lot (#58, spec US 25). It used
          to be one control meaning "give up every Avoid you have", so a
          Listener who mis-tapped one had to surrender the two-hour Avoid they
          meant in order to fix it — and had no way to see what they were
          holding. Clearing all is still offered, inside. */}
      {avoiding > 0 && (
        <div className="mt-2">
          <Control
            label={`Avoiding ${avoiding} talkgroups — show them`}
            onClick={() => setSheet('avoids')}
            icon={<Ban className="size-3.5" aria-hidden />}
          >
            Avoiding {avoiding}
          </Control>
        </div>
      )}

      <History
        calls={history}
        disabled={!can.recent}
        onReplay={(id) => dispatch(replay(id))}
      />

      {sheet === 'queue' && <QueueSheet onClose={() => setSheet(null)} />}
      {sheet === 'avoids' && <AvoidSheet onClose={() => setSheet(null)} />}
    </Screen>
  )
}

/**
 * The time now (#78) — rdio's clock, and a lost affordance: a scanner on a
 * shelf is also the clock in the room. Redrawn on the minute rather than every
 * second, because minutes are all it shows.
 */
function Clock() {
  const clock = useClock()
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const timer = setTimeout(() => setNow(Date.now()), 60_000 - (now % 60_000))
    return () => clearTimeout(timer)
  }, [now])
  return (
    <span aria-label="Time now" className="font-mono text-xs tabular-nums text-muted-foreground">
      {formatClock(now, clock)}
    </span>
  )
}

/** The scanner readout: who is talking — or who just was — on what, and where
 *  in the Call we are. */
function Display({
  call,
  ended,
  progress,
  paused,
  blocked,
  onListen,
  missed,
  gap,
}: {
  call: Call
  /** The transmission is over and this is the card it left behind (#56). Dimmed
   *  and said, because a finished Call that looked identical to one in progress
   *  would be the same lie in the other direction — and *said*, because the
   *  dimming is not something a screen reader can see. */
  ended: boolean
  progress: number
  paused: boolean
  /** Paused because the browser wants a tap first (#78) — which the card asks
   *  for, in place of claiming the Listener paused. */
  blocked: boolean
  onListen: () => void
  missed: number
  gap: boolean
}) {
  // A date that changes at midnight is all this card measures from the clock;
  // how late the Call was heard is measured once, as it started (#78).
  const now = useNow(true, DATE_TICK_MS)
  const late = useLateness(call, useAppSelector(selectPlayId))
  const color = ledForCall(call)

  return (
    // An ended card is dimmed where dimming says something — the LED and the
    // waveform go dark, and the name greys — rather than by fading the whole
    // card, which took every word on it below AA contrast (#78).
    <section
      aria-label="Scanner display"
      className="rounded-xl border border-border bg-card px-4 py-4"
    >
      <div className="flex items-start gap-3">
        {/* docs/design/brief.md state 6: paused blinks, playing is steady —
            and an ended Call is neither, so it does not blink either. */}
        <ChannelLed
          color={color}
          size={16}
          pulse={paused && !ended}
          className={cn('mt-1.5 transition-opacity', ended && 'opacity-40')}
        />
        <div className="min-w-0 flex-1">
          <p
            className={cn(
              'flex items-center gap-2 truncate font-mono text-lg leading-tight',
              ended && 'text-muted-foreground',
            )}
          >
            <span className="truncate">{talkgroupName(call)}</span>
            {/* An emergency has to be legible without reading anything (#42,
                spec US 5) — it is the flag #53 will also push on. */}
            <CallFlags call={call} />
          </p>
          <p className="truncate font-mono text-xs text-muted-foreground">
            {systemName(call)}
          </p>
          {callCategory(call) && (
            <p className="truncate font-mono text-[11px] text-muted-foreground">
              {callCategory(call)}
            </p>
          )}
        </div>
        {(ended || (paused && !blocked)) && (
          <span className="font-mono text-[11px] uppercase tracking-wider text-muted-foreground">
            {ended ? 'Ended' : 'Paused'}
          </span>
        )}
      </div>

      {/* The browser will not play sound before a tap (#78), and a first visit
          is exactly when it has had none — so the card asks for one, as the
          biggest thing on it, and says why. rdio's answer is a silent screen. */}
      {blocked && !ended && (
        <div className="mt-4 flex flex-col items-center gap-2">
          <Button className="h-11 w-full text-base" onClick={onListen}>
            <Play aria-hidden />
            Tap to listen
          </Button>
          <p className="text-xs text-muted-foreground">
            Your browser needs a tap before it will play sound.
          </p>
        </div>
      )}

      {/* Still, and back at the start: a waveform walking through a Call that
          has finished would be drawing audio nobody is hearing. */}
      <Waveform
        seed={call.id}
        color={color}
        progress={ended ? 0 : progress}
        live={!paused && !ended}
        className={cn('mt-4 transition-opacity', ended && 'opacity-40')}
      />

      <dl className="mt-4 grid grid-cols-2 gap-x-4 gap-y-2 font-mono text-[11px]">
        <Stat name="Frequency" testId="stat-frequency">
          {formatFrequency(call.frequency)}
        </Stat>
        <Stat name="TGID" testId="stat-talkgroup">
          {call.talkgroupRef}
        </Stat>
        {/* The radio that keyed, named where anybody has named it (#47, spec
            US 42), and a link to its history (US 44). rdio-scanner shows the
            bare number and nothing behind it. */}
        <Stat name="Unit" testId="stat-unit">
          {unitName(call) === undefined ? '—' : <UnitLink call={call} inline />}
        </Stat>
        <Stat name="Time" testId="stat-time">
          <CallTime ms={call.timestamp} now={now} />
        </Stat>
      </dl>

      {/* Honest timestamps (#78): a backlog, a Backfill, Catch-up and a Delay
          all play a Call minutes after it was said, and the time alone passes
          every one of them off as now. Amber — the colour of the feed's other
          states that are not a fault but are not live either. */}
      {late && (
        <p data-testid="lateness" className="mt-2 font-mono text-xs text-led-orange">
          {late}
        </p>
      )}

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
      <p className="max-w-xs text-xs text-muted-foreground">{empty.detail}</p>
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
 *
 * **And it is the way in** (#78, from #60): a link to Search on the gap, oldest
 * first, from where it began to now — and following it clears the count, which
 * is "what you have not seen yet". With nothing reaching a Listener while they
 * are away (ADR-0014), the Archive is the only answer to "what happened while I
 * wasn't looking", and this is the door to it. Open-ended rather than closed at
 * the moment it was drawn, so a link left on screen a while still reaches the
 * present.
 */
function Missed({ count, gap }: { count: number; gap: boolean }) {
  const dispatch = useAppDispatch()
  const from = useAppSelector(selectMissedFrom)
  if (count === 0 && !gap) return null
  const how = count > 0 ? `${count}${gap ? '+' : ''}` : 'some'
  // Nothing says when it began: newest first, where a Listener reads backwards
  // to find their place.
  const query = writeSearchUrl(
    from === undefined
      ? DEFAULT_SEARCH_URL
      : { search: { after: from, sort: 'oldest' }, offset: 0 },
  )
  return (
    <p className="mt-3" role="status">
      <Link
        to={query ? `/search?${query}` : '/search'}
        onClick={() => dispatch(missedCleared())}
        className="inline-flex min-h-11 items-center font-mono text-xs text-led-red underline decoration-dotted underline-offset-4"
      >
        {how} missed — see what you missed
      </Link>
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
      <dt className="text-[11px] uppercase tracking-wider text-muted-foreground">
        {name}
      </dt>
      <dd data-testid={testId} className="tabular-nums">
        {children}
      </dd>
    </div>
  )
}

/** One control. `label` is what it's called; the children are what fits.
 *  `hotkey` is the key that does the same from any tab (#78), said to a
 *  screen reader in `aria-keyshortcuts` and to a pointer in the title. */
function Control({
  label,
  hotkey,
  pressed,
  disabled,
  onClick,
  icon,
  children,
}: {
  label: string
  hotkey?: HotkeyCommand
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
      aria-keyshortcuts={hotkey && shortcutsFor(hotkey)}
      title={hotkey && `${label} (${shortcutsFor(hotkey).replaceAll(' ', ' or ')})`}
      disabled={disabled}
      onClick={onClick}
      className={cn(
        // 44 tall (#78): the size a thumb lands on without looking, which is
        // how a scanner's buttons are pressed.
        'h-11 gap-1.5 font-mono text-[11px] uppercase tracking-wider',
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
  const now = useNow(calls.length > 0, DATE_TICK_MS)
  if (calls.length === 0) return null

  return (
    <section className="mt-6">
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <h2 className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
          Recent
        </h2>
        {/* RECENT reaches back five (spec US 13); the session log reaches back
            to when the app was opened (#58, spec US 28). The way there is from
            the list it extends. */}
        <Link
          to="/session"
          className="-my-3 inline-flex min-h-11 items-center font-mono text-[11px] uppercase tracking-wider text-muted-foreground underline decoration-dotted underline-offset-2 transition-colors hover:text-foreground"
        >
          Session log
        </Link>
      </div>
      <ul
        aria-label="Recent calls"
        className="divide-y divide-border rounded-xl border border-border bg-card"
      >
        {calls.map((call) => (
          // The row is a list item holding a button, not a button holding
          // everything: the unit is a *link* (#47) and an anchor inside a button
          // is neither valid HTML nor reachable by a screen reader. Replaying is
          // still the whole row's width minus the two things beside it.
          <li key={`${call.id}`} className="flex items-center gap-2 px-3 py-0.5">
            <button
              type="button"
              disabled={disabled}
              onClick={() => onReplay(call.id)}
              className="flex min-h-11 min-w-0 flex-1 items-center gap-3 text-left transition-colors hover:bg-muted/40 disabled:opacity-40"
            >
              <ChannelLed
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
            </button>
            {/* Keeping one of the last five (#66, spec US 37). A sibling of
                the row's button and never inside it, the rule the comment above
                names.

                Dead with the feed off, like everything else on this screen
                (#88): a Listener who has switched off is shown one thing worth
                pressing, and a star that stayed live would be the second. The
                **session log** is where the last two hundred and fifty are
                starrable whatever the feed is doing, which is the screen for
                exactly that. */}
            <StarButton
              call={call}
              describedAs={talkgroupName(call)}
              disabled={disabled}
            />
            {/* Who keyed it, tappable straight through to that radio's history
                (#47, spec US 44) — "who was that ten minutes ago" answered from
                the list it happened in. */}
            <UnitLink
              call={call}
              className="max-w-24 shrink-0 font-mono text-[11px] text-muted-foreground"
            />
            <CallTime
              ms={call.timestamp}
              now={now}
              className="shrink-0 font-mono text-[11px] text-muted-foreground"
            />
          </li>
        ))}
      </ul>
    </section>
  )
}
