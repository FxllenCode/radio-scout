import { Screen } from '@/components/layout/Screen'

/** Search (Archive) — browse/replay stored calls with cascading filters and
 *  playback mode (#13). */
export function SearchScreen() {
  return (
    <Screen title="Search">
      <div className="rounded-xl border border-border bg-card px-6 py-12 text-center">
        <p className="font-mono text-sm text-muted-foreground">Archive search</p>
        <p className="mt-1 text-xs text-muted-foreground/70">
          Filter stored calls by date, system, talkgroup, group, and tag.
        </p>
      </div>
    </Screen>
  )
}
