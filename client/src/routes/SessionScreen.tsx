/**
 * The session log (#58, spec US 28) — everything heard since the app opened,
 * with replay and the quick actions a Listener reaches for when they finally
 * work out what they heard.
 *
 * # Why a route rather than a sheet
 *
 * The Live screen's RECENT list reaches back five Calls (spec US 13), and "what
 * was that ten minutes ago" is further back than that on any system worth
 * monitoring. This is a list of up to 250, read by scrolling — so it gets a
 * screen, the way `UnitScreen` does, rather than a panel over the one it was
 * opened from. Like that screen it is a route and not a tab: it is always
 * arrived at *from* the Live screen, never browsed to.
 *
 * # Why the actions are behind a long press
 *
 * A row's ordinary meaning is *play this again*, which is the only thing most
 * taps here want. Putting Hold, Avoid and Download on the row itself would make
 * every row four controls wide on a phone, and would put *Avoid* — which
 * silences a channel — a thumb's width from the thing a Listener actually
 * meant. Held, they open a sheet naming the Call, so what is about to be
 * silenced is on screen before it happens.
 *
 * The press acts on the Call the finger went down on (`useLongPress`), which is
 * not decoration: this list grows at the top as Calls are heard, so the row
 * under a thumb genuinely moves mid-gesture.
 */
import { Ban, Download, Radio, RotateCcw, Share2 } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router-dom'

import { CallFlags } from '@/components/CallFlags'
import { Screen } from '@/components/layout/Screen'
import { Sheet } from '@/components/Sheet'
import { StatusLed } from '@/components/StatusLed'
import { UnitLink } from '@/components/UnitLink'
import { useLongPress } from '@/hooks/useLongPress'
import { usePublicShare } from '@/hooks/usePublicShare'
import { useShareLink } from '@/hooks/useShareLink'
import { downloadUrl, formatCallTime } from '@/lib/archive'
import { systemName, talkgroupName } from '@/lib/call'
import { feedPlays } from '@/lib/feed'
import { ledForCall } from '@/lib/led'
import { cn } from '@/lib/utils'
import { useGetCatalogQuery } from '@/store/api'
import { useAppDispatch, useAppSelector } from '@/store/hooks'
import {
  avoidTalkgroup,
  replay,
  selectFeedStatus,
  selectHold,
  selectSessionLog,
  toggleHoldOn,
} from '@/store/live'
import type { Call } from '@/types'

export function SessionScreen() {
  const dispatch = useAppDispatch()
  const calls = useAppSelector(selectSessionLog)
  const hold = useAppSelector(selectHold)
  // Replaying puts audio on the element, which is exactly what a Listener who
  // chose silence did not ask for (#80, #88) — `replay` refuses in the reducer
  // too. Nothing else this screen offers is audio, which is the distinction the
  // rows are built around.
  const plays = feedPlays(useAppSelector(selectFeedStatus))
  const [acting, setActing] = useState<Call | null>(null)
  // Minting a public link is two steps that are one gesture (#64), and the
  // catalog is what says whether this Instance mints them at all.
  const link = useShareLink()
  const sharePublicly = usePublicShare(link)
  const { data: catalog } = useGetCatalogQuery()

  // **`aria-disabled`, never `disabled`.** A disabled button fires no pointer
  // events, so gating the row that way would take *Hold*, *Avoid* and
  // *Download* with it — none of which is audio, and all of which are exactly
  // what a Listener browsing with the feed off is here for. So the row stays
  // pressable, says it cannot replay, and the tap is what is refused.
  const press = useLongPress<Call>({
    onTap: (call) => {
      if (plays) dispatch(replay(call.id))
    },
    onHold: setActing,
  })

  return (
    <Screen
      title="SESSION"
      status={
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
          {calls.length} heard
        </span>
      }
    >
      <p className="mb-4 text-xs text-muted-foreground/70">
        Everything heard since this app was opened.{' '}
        {plays ? 'Tap to replay; press' : 'Press'} and hold for hold, avoid,
        share and download.
      </p>

      {/* What a share control just did. Copying has no visible result at all
          and failing to copy leaves somebody waiting for a link that is not
          coming, so both are said — and `useShareLink` takes the saying back
          down again, which is the half a second copy of this forgot (#61). */}
      {link.notice && (
        <p
          role="status"
          className="mb-3 rounded-md border border-border bg-card px-3 py-2 font-mono text-[11px] text-muted-foreground"
        >
          {link.notice}
        </p>
      )}

      {calls.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-xl border border-border bg-card px-6 py-12 text-center">
          <Radio className="size-6 text-muted-foreground" aria-hidden />
          <p className="font-mono text-sm text-muted-foreground">Nothing heard yet</p>
          <p className="max-w-xs text-xs text-muted-foreground/70">
            Calls appear here as they play. The archive holds everything from
            before this session — <Link to="/search" className="underline">search it</Link>.
          </p>
        </div>
      ) : (
        <ul
          aria-label="Session log"
          className="divide-y divide-border rounded-xl border border-border bg-card"
        >
          {/* Keyed by Call id, so a Call arriving at the top moves each row's
              node rather than rewriting it — which is what keeps a finger on
              the row it went down on. */}
          {calls.map((call) => (
            <li key={call.id} className="flex items-center gap-3 px-3 py-2.5">
              <button
                type="button"
                aria-disabled={!plays}
                {...press(call)}
                className={cn(
                  'flex min-w-0 flex-1 items-center gap-3 text-left transition-colors hover:bg-muted/40',
                  !plays && 'opacity-40',
                )}
              >
                <StatusLed color={ledForCall(call)} size={10} />
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="flex min-w-0 items-center gap-1.5 truncate font-mono text-sm leading-tight">
                    <span className="truncate">{talkgroupName(call)}</span>
                    <CallFlags call={call} />
                  </span>
                  <span className="truncate font-mono text-[10px] text-muted-foreground">
                    {systemName(call)}
                  </span>
                </span>
              </button>
              <UnitLink
                call={call}
                className="max-w-24 shrink-0 font-mono text-[11px] text-muted-foreground"
              />
              <time className="shrink-0 font-mono text-[11px] text-muted-foreground">
                {formatCallTime(call.timestamp)}
              </time>
            </li>
          ))}
        </ul>
      )}

      {acting && (
        <Sheet title={talkgroupName(acting)} onClose={() => setActing(null)}>
          <div className="grid gap-2">
            <Action
              icon={<RotateCcw className="size-4" aria-hidden />}
              disabled={!plays}
              onClick={() => {
                dispatch(replay(acting.id))
                setActing(null)
              }}
            >
              Replay
            </Action>
            <Action
              icon={<Radio className="size-4" aria-hidden />}
              onClick={() => {
                dispatch(
                  toggleHoldOn({
                    systemRef: acting.systemRef,
                    talkgroupRef: acting.talkgroupRef,
                  }),
                )
                setActing(null)
              }}
            >
              {hold?.systemRef === acting.systemRef &&
              hold.talkgroupRef === acting.talkgroupRef
                ? 'Release hold'
                : 'Hold this talkgroup'}
            </Action>
            <Action
              icon={<Ban className="size-4" aria-hidden />}
              onClick={() => {
                dispatch(
                  avoidTalkgroup({
                    systemRef: acting.systemRef,
                    talkgroupRef: acting.talkgroupRef,
                    until: 0,
                  }),
                )
                setActing(null)
              }}
            >
              Avoid this talkgroup
            </Action>
            {/* **Where a moment is shared from** (#64, spec US 32). This screen
                is what a Listener reaches for after hearing something — the
                Archive is where you go when you have to *look* for it — so the
                public link belongs here as much as on a search result. Offered
                on an encrypted Call too: that the channel was busy is the fact
                worth sending. Absent where the Instance does not mint links,
                because a control that is offered and then refused lies. */}
            {catalog?.sharing && (
              <Action
                icon={<Share2 className="size-4" aria-hidden />}
                onClick={() => {
                  void sharePublicly(acting)
                  setActing(null)
                }}
              >
                Share a public link
              </Action>
            )}
            {/* An anchor, not a button: the browser owns saving a file, and an
                encrypted Call has no audio to offer (#42). */}
            {acting.audioUrl && (
              <a
                href={downloadUrl(acting.id)}
                download
                onClick={() => setActing(null)}
                className="flex items-center gap-3 rounded-lg border border-border px-3 py-2.5 font-mono text-xs transition-colors hover:bg-muted/40"
              >
                <Download className="size-4" aria-hidden />
                Download
              </a>
            )}
          </div>
        </Sheet>
      )}
    </Screen>
  )
}

function Action({
  icon,
  disabled,
  onClick,
  children,
}: {
  icon: React.ReactNode
  disabled?: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="flex items-center gap-3 rounded-lg border border-border px-3 py-2.5 text-left font-mono text-xs transition-colors hover:bg-muted/40 disabled:opacity-40"
    >
      {icon}
      {children}
    </button>
  )
}
