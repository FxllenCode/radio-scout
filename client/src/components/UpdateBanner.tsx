import { DockedBanner } from '@/components/layout/DockedBanner'
import { Button } from '@/components/ui/button'

/**
 * The "a new version is waiting" banner (#15).
 *
 * The waiting is the point. Radio-Scout is a thing you leave running — a worker
 * that took over and reloaded on its own would cut off a Call mid-sentence and,
 * on a phone, end the backgrounded listening session ADR-0005 exists to make
 * work. So a deploy waits here, above the tab bar, until the listener has a
 * moment. Ignoring it costs nothing: it applies on the next cold start.
 */
export function UpdateBanner({ apply }: { apply: () => void }) {
  return (
    <DockedBanner>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-semibold">A new version is ready</p>
        <p className="text-xs text-muted-foreground">
          It installs when you reload — nothing is interrupted until then.
        </p>
      </div>
      <Button size="sm" variant="secondary" onClick={apply}>
        Reload
      </Button>
    </DockedBanner>
  )
}
