import { useState } from 'react'

import { useOnline } from '@/hooks/useOnline'
import { callAnnouncement, feedAnnouncement } from '@/lib/announce'
import { useAppSelector } from '@/store/hooks'
import { selectFeedStatus, selectPlayId, selectRefused } from '@/store/live'
import { selectIsLiveSource, selectNowPlaying } from '@/store/transport'

/** What was last true, so a change can be told apart from a re-render. */
interface Seen {
  /** The Call on the element, by id — never by identity: a Call is a new
   *  object every time its Quiet spans land (#59), and that is not a new Call. */
  call: number | null
  /** Bumped on every (re)start of a live Call, which is what makes a replay of
   *  the same Call a second announcement. */
  playId: number
  fromArchive: boolean
  feed: string
}

/**
 * The screen-reader half of the scanner readout (#78).
 *
 * One polite, atomic live region in the shell — above the router, beside the
 * `<audio>` element — because what it reads out follows the Listener between
 * tabs (#56). It says two things: each Call as it starts, and the feed's
 * standing when its *cause* changes ([`feedAnnouncement`]). Neither is said
 * for the state the page opened in, which is not news.
 *
 * Each announcement is a fresh node. A live region whose text is the same as
 * last time is not read again, and the same Talkgroup twice in a row — a replay,
 * or a dispatcher keying twice — is two things that happened.
 */
export function Announcer() {
  const playing = useAppSelector(selectNowPlaying)
  const fromArchive = !useAppSelector(selectIsLiveSource)
  const playId = useAppSelector(selectPlayId)
  const status = useAppSelector(selectFeedStatus)
  const refused = useAppSelector(selectRefused)
  const offline = !useOnline()
  const now: Seen = {
    call: playing?.id ?? null,
    playId,
    fromArchive,
    feed: feedAnnouncement(status, { refused, offline }),
  }

  const [seen, setSeen] = useState(now)
  const [said, setSaid] = useState<{ text: string; count: number } | null>(null)
  // Adjusted while rendering, React's own pattern for state that follows a
  // value — so the announcement lands in the same commit as the Call it names.
  if (
    now.call !== seen.call ||
    now.playId !== seen.playId ||
    now.fromArchive !== seen.fromArchive ||
    now.feed !== seen.feed
  ) {
    setSeen(now)
    const started =
      playing !== null &&
      (now.call !== seen.call || now.playId !== seen.playId || now.fromArchive !== seen.fromArchive)
    const text = started
      ? callAnnouncement(playing, fromArchive)
      : now.feed !== seen.feed
        ? now.feed
        : null
    if (text) setSaid((last) => ({ text, count: (last?.count ?? 0) + 1 }))
  }

  return (
    <div data-testid="announcer" aria-live="polite" aria-atomic="true" className="sr-only">
      {said && <span key={said.count}>{said.text}</span>}
    </div>
  )
}
