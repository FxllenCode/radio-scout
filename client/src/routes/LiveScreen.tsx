import { Screen } from '@/components/layout/Screen'
import { StatusLed } from '@/components/StatusLed'
import { Button } from '@/components/ui/button'
import { LED_ORDER } from '@/lib/led'

/** Live scanner (home) — the hero. This scaffold shows the idle/empty state and
 *  the visual language (LED palette, mono readouts); the full scanner display,
 *  waveform, and controls land in #11, wired to live-feed state. */
export function LiveScreen() {
  return (
    <Screen
      title="LIVE"
      status={
        <span className="inline-flex items-center gap-1.5">
          <StatusLed color="green" size={8} pulse />
          connected
        </span>
      }
    >
      <div className="flex flex-col items-center gap-3 rounded-xl border border-border bg-card px-6 py-12 text-center">
        <StatusLed color="white" size={16} />
        <p className="font-mono text-sm text-muted-foreground">
          Waiting for the first call…
        </p>
        <p className="max-w-xs text-xs text-muted-foreground/70">
          Point a Trunk Recorder or SDRTrunk instance at this server and calls
          for your selected talkgroups will play here automatically.
        </p>
      </div>

      <div className="mt-4 grid grid-cols-3 gap-2 opacity-50">
        <Button variant="outline" size="sm" disabled>
          Hold Sys
        </Button>
        <Button variant="outline" size="sm" disabled>
          Skip
        </Button>
        <Button variant="outline" size="sm" disabled>
          Avoid
        </Button>
      </div>

      <section className="mt-8">
        <h2 className="mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground">
          LED palette
        </h2>
        <ul className="grid grid-cols-4 gap-3">
          {LED_ORDER.map((color) => (
            <li key={color} className="flex flex-col items-center gap-1.5">
              <StatusLed color={color} size={14} />
              <span className="font-mono text-[10px] text-muted-foreground">
                {color}
              </span>
            </li>
          ))}
        </ul>
      </section>
    </Screen>
  )
}
