import {
  Gauge,
  Link2,
  Pause,
  Play,
  SkipBack,
  SkipForward,
  Square,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useSearchParams } from 'react-router-dom'

import { CallFlags } from '@/components/CallFlags'
import { DensityRibbon } from '@/components/DensityRibbon'
import { DateField, Field, controlClass } from '@/components/Field'
import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { UnitLink } from '@/components/UnitLink'
import { Button } from '@/components/ui/button'
import { useRunPageAhead } from '@/hooks/useRunPageAhead'
import { useShareLink } from '@/hooks/useShareLink'
import { formatCallTime, formatDuration } from '@/lib/archive'
import { CATCHUP_RATE } from '@/lib/catchup'
import { systemName, talkgroupName } from '@/lib/call'
import { PRESETS, rangeOf } from '@/lib/dateRange'
import { bucketOfInstant, bucketStartMs } from '@/lib/density'
import {
  channelScope,
  dvrActivity,
  dvrSearch,
  playheadMs,
  readDvrUrl,
  soleChannel,
  writeDvrUrl,
  type DvrView,
} from '@/lib/dvr'
import { ledForCall } from '@/lib/led'
import { sameSearch } from '@/lib/run'
import { encodeSelection } from '@/lib/selectionUrl'
import {
  useGetActivityQuery,
  useGetFilterOptionsQuery,
  useLazySearchCallsQuery,
} from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import { selectSelection } from '@/store/live'
import {
  enterPlaybackMode,
  next,
  previous,
  searchChanged,
  selectCurrentCall,
  selectHasNext,
  selectHasPrevious,
  selectPlaybackPosition,
  selectRunHurrying,
  startRun,
  stop,
  toggleHurry,
} from '@/store/playback'
import {
  seekTo,
  selectIsPaused,
  selectPlayhead,
  togglePause,
} from '@/store/transport'

/** How many Calls a DVR page holds — the Search screen's, because it is the
 *  same **Run** walking the same archive. */
const PAGE_SIZE = 50

/** The scope picker's value for a Selection somebody else built: neither one
 *  channel nor this Listener's own scanner. Never chosen — see the option. */
const SHARED = 'shared'

/**
 * The **DVR** (#63, spec US 39–40): rewind a channel — or the whole scanner —
 * across a stretch of time and play it forwards, gaplessly, on a scrubbable
 * density timeline.
 *
 * No product has one. rdio-scanner's idea of time travel is the Previous
 * button, one page of search results at a time.
 *
 * # It is a Run, and it costs the Pi nothing new
 *
 * CONTEXT.md: walking search results, playing forward from a row and a DVR are
 * "the same **Run** configured differently". So this screen starts an ordinary
 * oldest-first Run and everything about walking it — the page-ahead, the
 * roll-on, what plays next, the prefetch — is `lib/run`'s already.
 *
 * That is also the whole of the ticket's serving criterion. The "playlist" is
 * the ordered page of Calls the archive already answers with, each carrying the
 * URL of an object the server already serves; there is no stitching, no
 * transcoding and no concatenation on the serve path, because there is no serve
 * path that did not exist before. What a listener hears as *gapless* is the
 * recorder's own dead air being trimmed out ([`toggleHurry`]) plus #14's
 * prefetch of the Call behind the one playing.
 *
 * A real HLS playlist was the other reading and is the wrong one: recorder
 * objects are arbitrary WAV and MP3 at whatever the recorder chose — a WAV is
 * not a valid HLS segment, and Trunk Recorder uploads WAV by default — and
 * ADR-0005's revised ladder puts Managed Media Source out of reach anyway,
 * because anything that would make it viable makes the **Station stream**
 * (#74) strictly better.
 *
 * # Its position is a time
 *
 * The Search screen's ribbon scrubs by moving a window of *results* (#62) and
 * deliberately leaves the Run alone. Here the two are the same thing: "rewind
 * the county to 2am" names an instant, so a scrub re-anchors the Run and the
 * marker is drawn from where the playhead actually is. `lib/dvr` owns all of
 * that arithmetic; this screen draws it and listens.
 */
export function DvrScreen() {
  const dispatch = useAppDispatch()
  const [params, setParams] = useSearchParams()

  /**
   * The scanner the Listener arrived with, and the moment they arrived.
   *
   * Both frozen, and for one reason: they are *defaults*, and a default that
   * moved would change the search behind a **Run** that is walking — ending it
   * (#89) because somebody toggled a Talkgroup on another screen, or because a
   * minute went by and "the last hour" became a different hour.
   */
  const scanner = useAppSelector(selectSelection)
  const [arrivedWith] = useState(scanner)
  const [openedAt] = useState(() => Date.now())

  /** Everything this screen is showing, read off the URL. Memoized on the query
   *  *string* so an unchanged view keeps its identity — it is an RTK Query
   *  argument and an effect dependency both. */
  const query = params.toString()
  const view = useMemo(
    () => readDvrUrl(new URLSearchParams(query), arrivedWith, openedAt),
    [query, arrivedWith, openedAt],
  )

  const search = useMemo(() => dvrSearch(view), [view])
  /** How busy this scope was across the whole range — the timeline, and the
   *  only request this screen makes that a scrub does not change. */
  const { data: timeline } = useGetActivityQuery(dvrActivity(view))
  /** Every channel there is, for the picker. Deliberately unscoped: a picker
   *  narrowed by the scope currently chosen could never be used to choose a
   *  different one. */
  const { data: options } = useGetFilterOptionsQuery({})

  const current = useAppSelector(selectCurrentCall)
  const position = useAppSelector(selectPlaybackPosition)
  const paused = useAppSelector(selectIsPaused)
  const hurrying = useAppSelector(selectRunHurrying)
  const hasNext = useAppSelector(selectHasNext)
  const hasPrevious = useAppSelector(selectHasPrevious)
  const playhead = useAppSelector(selectPlayhead)

  const link = useShareLink()

  /** Where the URL is going next. A scrub *replaces* — the search has not
   *  changed and Back should return to before the drag rather than through
   *  every bucket it passed — where a change of scope or range is a new view
   *  and earns an entry. */
  const goTo = (patch: Partial<DvrView>, replace = false) =>
    setParams(writeDvrUrl({ ...view, ...patch }), { replace })

  /** Any change of scope or range re-anchors at the start of the new range:
   *  the old anchor described a stretch of time that may no longer be in it. */
  const rescope = (patch: Partial<DvrView>) => {
    const next = { ...view, ...patch }
    goTo({ ...patch, at: next.from })
  }

  /**
   * Tell the **Run** the search changed — observed here rather than dispatched
   * from each control that could cause one, which is the only way the back
   * button gets the same treatment as a dropdown (#61's argument).
   */
  const lastSearch = useRef(search)
  useEffect(() => {
    if (sameSearch(lastSearch.current, search)) return
    lastSearch.current = search
    dispatch(searchChanged(search))
  }, [dispatch, search])

  // Every screen that starts a Run owes the page-ahead (#47's lesson); without
  // it a DVR plays fifty Calls and stops, which looks exactly like the night
  // having been quiet.
  useRunPageAhead()

  /**
   * Start playing at `at` — which is what both the play button and a scrub do.
   *
   * Fetched lazily rather than subscribed to, so the screen holds no page of
   * its own: the Run holds the one it is walking, and this is only ever the
   * first of them. That is also what makes a scrub a *jump* rather than a
   * search change the Run would merely be disarmed by.
   */
  const [fetchRun] = useLazySearchCallsQuery()
  async function playFrom(at: number) {
    const from = dvrSearch({ ...view, at })
    try {
      const page = await fetchRun({ ...from, limit: PAGE_SIZE, offset: 0 }).unwrap()
      // The first Call there is something to play. An encrypted one carries no
      // audio at all (spec US 9), and a Run started on one would be stuck
      // before it began.
      const index = page.results.findIndex((call) => call.audioUrl)
      if (index < 0) {
        link.say('Nothing to play from there.')
        return
      }
      // Playback mode first, or the Call would *interrupt* the live feed as a
      // single Call rather than starting a walk (spec US 25 vs 26).
      dispatch(enterPlaybackMode())
      dispatch(startRun({ search: from, page, index }))
    } catch {
      link.say('Could not read the archive from there.')
    }
  }

  /** Where the playhead is, as an instant — the marker's own fact. */
  const playingAt = playheadMs(view, current, playhead.position)
  const channel = soleChannel(view.scope)
  const mine = encodeSelection(view.scope) === encodeSelection(arrivedWith)
  const scopeValue = channel
    ? `${channel.systemRef}:${channel.talkgroupRef}`
    : mine
      ? ''
      : SHARED

  return (
    <Screen title="DVR">
      <p className="font-mono text-[11px] text-muted-foreground">
        Rewind a channel — or your whole scanner — and play it forwards.
      </p>

      <form
        aria-label="DVR scope and range"
        className="mt-4 grid grid-cols-2 gap-2"
        onSubmit={(event) => event.preventDefault()}
      >
        {/* One picker, and what it produces is always a **Selection** — a
            single channel is a one-entry matrix (`lib/dvr`), so the DVR has
            one scoping rule rather than two that differ on **Patch**es. */}
        <div className="col-span-2">
          <Field label="Scope" htmlFor="dvr-scope">
            <select
              id="dvr-scope"
              className={controlClass}
              value={scopeValue}
              onChange={(event) => {
                if (!event.target.value) {
                  rescope({ scope: arrivedWith })
                  return
                }
                const [systemRef, talkgroupRef] = event.target.value.split(':')
                rescope({
                  scope: channelScope(Number(systemRef), Number(talkgroupRef)),
                })
              }}
            >
              {/* A link may carry a Selection that is neither a single channel
                  nor this Listener's own — one somebody else built. Showing
                  "My scanner" for it would be a straightforwardly false label
                  on the control that says what is playing, so it gets a
                  placeholder of its own. Disabled, so it is never something to
                  *choose*: it describes where the Listener already is. */}
              {!channel && !mine && (
                <option value={SHARED} disabled>
                  A shared selection
                </option>
              )}
              <option value="">My scanner</option>
              {options?.talkgroups.map((talkgroup) => (
                <option
                  key={`${talkgroup.systemRef}:${talkgroup.ref}`}
                  value={`${talkgroup.systemRef}:${talkgroup.ref}`}
                >
                  {talkgroup.label ?? talkgroup.ref}
                </option>
              ))}
            </select>
          </Field>
        </div>

        <DateField
          label="From"
          id="dvr-from"
          ms={view.from}
          onChange={(from) => from !== undefined && rescope({ from })}
        />
        <DateField
          label="To"
          id="dvr-to"
          ms={view.to}
          onChange={(to) => to !== undefined && rescope({ to })}
        />

        <div className="col-span-2 flex flex-wrap items-center gap-1.5">
          {PRESETS.map((preset) => (
            <Button
              key={preset.id}
              type="button"
              variant="outline"
              size="sm"
              className="h-7 px-2 font-mono text-[10px] uppercase tracking-wider"
              onClick={() => {
                const { after, before } = rangeOf(preset.id, Date.now())
                rescope({ from: after, to: before })
              }}
            >
              {preset.label}
            </Button>
          ))}
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label="Copy link to this DVR"
            className="ml-auto h-7 px-2"
            onClick={() => link.share('/dvr', writeDvrUrl(view), 'Radio-Scout DVR')}
          >
            <Link2 className="size-3.5" aria-hidden />
          </Button>
        </div>
      </form>

      {link.notice && (
        <p
          role="status"
          className="mt-3 rounded-md border border-border bg-card px-3 py-2 font-mono text-[11px] text-muted-foreground"
        >
          {link.notice}
        </p>
      )}

      {/* The timeline. Its marker is the *playhead*, not a page offset — which
          is the whole difference between this and the Search screen's ribbon,
          and why `DensityRibbon` takes a resolved bucket rather than working
          one out from a window. */}
      <section aria-label="Timeline" className="mt-5 space-y-2">
        {timeline ? (
          <DensityRibbon
            series={timeline}
            at={bucketOfInstant(timeline, playingAt)}
            label="Rewind to a time in this range"
            onJump={(bucket) => {
              const at = bucketStartMs(timeline, bucket)
              goTo({ at }, true)
              // Only jump what is already playing. Scrubbing before pressing
              // play is choosing where to start, not starting.
              if (current) void playFrom(at)
            }}
          />
        ) : (
          <p className="font-mono text-[11px] text-muted-foreground">
            Reading the archive…
          </p>
        )}
      </section>

      <section
        aria-label="DVR transport"
        className="mt-4 rounded-xl border border-border bg-card px-3 py-2.5"
      >
        <div className="flex items-center gap-3">
          {/* Nothing playing gets no LED rather than a made-up one: the
              colour carries meaning here (docs/design/brief.md) and grey
              would be a talkgroup that had one. */}
          {current && (
            <StatusLed color={ledForCall(current)} size={12} pulse={paused} />
          )}
          <div className="min-w-0 flex-1">
            <p className="flex items-center gap-1.5 truncate font-mono text-sm">
              <span className="truncate">
                {current ? talkgroupName(current) : 'Nothing playing'}
              </span>
              {current && <CallFlags call={current} />}
            </p>
            <p className="truncate font-mono text-[11px] text-muted-foreground">
              {current
                ? `${systemName(current)} · ${formatCallTime(playingAt)} · ${
                    position.index + 1
                  } of ${position.total}`
                : `${formatCallTime(view.at)} onwards`}
            </p>
          </div>
          {current && (
            <span className="w-20 shrink-0 truncate text-right font-mono text-[11px] text-muted-foreground">
              <UnitLink call={current} />
            </span>
          )}
        </div>

        {/* Seeking *within* the Call on the element. The browser asks for the
            bytes it does not have with a range request, which `src/serve.rs`
            has answered since #10 — so this costs the Pi nothing it was not
            already doing. */}
        <div className="mt-3 flex items-center gap-2">
          <input
            type="range"
            aria-label="Seek within this call"
            min={0}
            max={playhead.duration || 1}
            step={0.1}
            value={Math.min(playhead.position, playhead.duration || 1)}
            disabled={!current || playhead.duration === 0}
            onChange={(event) => dispatch(seekTo(Number(event.target.value)))}
            className="h-1 w-full accent-foreground"
          />
          <span className="w-12 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
            {formatDuration(playhead.position * 1000)}
          </span>
        </div>

        <div className="mt-3 flex items-center gap-2">
          <Button
            variant="outline"
            size="icon"
            aria-label="Previous call"
            disabled={!hasPrevious}
            onClick={() => dispatch(previous())}
          >
            <SkipBack className="size-4" aria-hidden />
          </Button>
          {current ? (
            <Button
              variant="outline"
              size="icon"
              aria-label={paused ? 'Resume' : 'Pause'}
              onClick={() => dispatch(togglePause())}
            >
              {paused ? (
                <Play className="size-4" aria-hidden />
              ) : (
                <Pause className="size-4" aria-hidden />
              )}
            </Button>
          ) : (
            <Button
              variant="outline"
              size="icon"
              aria-label="Play from here"
              onClick={() => void playFrom(view.at)}
            >
              <Play className="size-4" aria-hidden />
            </Button>
          )}
          <Button
            variant="outline"
            size="icon"
            aria-label="Next call"
            disabled={!hasNext}
            onClick={() => dispatch(next())}
          >
            <SkipForward className="size-4" aria-hidden />
          </Button>
          <Button
            variant="outline"
            size="icon"
            aria-label="Stop"
            disabled={!current}
            onClick={() => dispatch(stop())}
          >
            <Square className="size-4" aria-hidden />
          </Button>
          {/* Both of **Catch-up**'s levers, as one control, because they are one
              decision (`drain`'s single parameter). Deliberately not called
              Catch-up: that word is draining the listening queue and ends when
              the queue empties, where this ends at the end of a range. */}
          <Button
            variant={hurrying ? 'secondary' : 'outline'}
            size="sm"
            aria-pressed={hurrying}
            className="ml-auto h-9 gap-1 px-2 font-mono text-[10px] uppercase tracking-wider"
            onClick={() => dispatch(toggleHurry())}
          >
            <Gauge className="size-3.5" aria-hidden />
            {CATCHUP_RATE}× · skip quiet
          </Button>
        </div>
      </section>
    </Screen>
  )
}
