import { Screen } from '@/components/layout/Screen'

/** Talkgroups (Select) — the live-feed selection surface (#12): group/tag
 *  category toggles, per-system talkgroup lists, all-on/all-off. */
export function TalkgroupsScreen() {
  return (
    <Screen title="Talkgroups">
      <div className="rounded-xl border border-border bg-card px-6 py-12 text-center">
        <p className="font-mono text-sm text-muted-foreground">
          No systems yet.
        </p>
        <p className="mt-1 text-xs text-muted-foreground/70">
          Systems and talkgroups appear here automatically as calls arrive.
        </p>
      </div>
    </Screen>
  )
}
