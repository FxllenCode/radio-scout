import { useAppDispatch, useAppSelector } from '@/store/hooks'
import { next, selectCurrentCall } from '@/store/playback'

/**
 * The app's one `<audio>` element (ADR-0005: a single reused element, never
 * WebAudio — it is what keeps audio alive when a phone is backgrounded).
 *
 * It lives in the shell rather than in a screen so playback survives navigating
 * between tabs. Today it plays whatever the archive queue points at and
 * advances on `ended`, which is what makes playback mode play *sequentially*
 * (#13, spec US 25). #14 grows this same element into the full player: Media
 * Session metadata and lock-screen controls, next-Call prefetch, and the live
 * feed as a second source.
 *
 * `autoPlay` rather than an imperative `play()`: changing `src` on an autoplay
 * element starts it, so there is no promise to swallow and no divergence
 * between the first Call and the rest.
 */
export function CallPlayer() {
  const dispatch = useAppDispatch()
  const current = useAppSelector(selectCurrentCall)

  return (
    <audio
      data-testid="call-player"
      autoPlay
      src={current?.audioUrl}
      onEnded={() => dispatch(next())}
    />
  )
}
