import { useState } from 'react'

import { usePush } from '@/hooks/usePush'
import type { Push, PushState } from '@/lib/push'
import { needsHomeScreen } from '@/lib/install'
import { useAppSelector } from '@/store/hooks'
import { selectSubscription } from '@/store/transport'

/**
 * The Web Push switch (#16, spec US 32).
 *
 * The whole reason this is a switch on a settings screen rather than a prompt
 * on first load: `Notification.requestPermission()` can be spent **once** per
 * origin. Asked out of nowhere it is refused, and on iOS the only way back is
 * deleting the home-screen app. So it is asked on a tap, and every state before
 * that tap has to be explicable in one line of text.
 */
export function NotificationsRow() {
  const { push, state } = usePush()
  // A subscribe is a round trip and a row write; the switch is disabled until
  // it lands rather than queueing a second one behind it.
  const [busy, setBusy] = useState(false)
  // What the listener hears is what they will be told about — the same matrix
  // the live socket carries, so an avoided Talkgroup (spec US 14) is quiet on
  // the lock screen too.
  const selection = useAppSelector(selectSubscription)

  const on = state === 'on'
  const toggle = async (handle: Push) => {
    setBusy(true)
    try {
      if (on) await handle.disable()
      else await handle.enable(selection)
    } finally {
      setBusy(false)
    }
  }

  return (
    <li className="flex items-center justify-between px-4 py-3.5">
      <span className="text-sm">Notifications</span>
      {push && (state === 'off' || state === 'on') ? (
        <button
          type="button"
          role="switch"
          aria-checked={on}
          aria-label="Notifications"
          disabled={busy}
          onClick={() => void toggle(push)}
          className={`relative h-6 w-11 shrink-0 rounded-full border transition-colors ${
            on ? 'border-primary bg-primary' : 'border-border bg-muted'
          } disabled:opacity-50`}
        >
          <span
            className={`absolute top-0.5 size-4 rounded-full bg-background transition-all ${
              on ? 'left-[1.375rem]' : 'left-0.5'
            }`}
          />
        </button>
      ) : (
        <span className="font-mono text-xs text-muted-foreground/60">
          {explain(state)}
        </span>
      )}
    </li>
  )
}

/** Why the switch isn't there — in the listener's terms, and actionable where
 *  there is an action. Total over every state, so the caller never has to
 *  launder one it knows cannot arrive. */
function explain(state: PushState): string {
  if (state === 'blocked') return 'blocked'
  if (state === 'unavailable') return 'unavailable'
  // iOS offers Web Push to home-screen apps and to nothing else (research §6),
  // so on the one browser where that is the answer, say the thing that fixes
  // it rather than "unsupported".
  return needsHomeScreen() ? 'add to Home Screen' : 'unsupported'
}
