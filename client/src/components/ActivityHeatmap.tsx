import { DAY_LABELS, heatmapOf, isHourly } from '@/lib/heatmap'
import type { Series } from '@/types'

/**
 * **When the county gets busy** (#62) — a week of hours, shaded by traffic.
 *
 * The second view of the same search: [`DensityRibbon`] shows *when* it was
 * busy, and this shows *when it usually is*, which is the question you cannot
 * answer by looking at a timeline of one particular fortnight.
 *
 * Cells are not tappable, and that is not an omission. A cell is "every Tuesday
 * at 09:00", which is not a moment — there is no archive to open at it. The
 * ribbon is where you land somewhere; this is where you learn where to look.
 *
 * The fold into local days and hours happens in `@/lib/heatmap`, in the browser,
 * because the server has no idea what timezone the Listener is in.
 */
export function ActivityHeatmap({ series }: { series: Series }) {
  // The server widens a grain that would not fit its own bound and says so in
  // the answer. Folding a widened series would attribute three hours of traffic
  // to whichever hour it began in — a confidently wrong grid with nothing on
  // screen to say so — so a range this build did not expect gets a sentence
  // instead of a picture.
  if (!isHourly(series))
    return (
      <p className="font-mono text-[11px] text-muted-foreground">
        That range is too long to break into hours. Narrow the dates to see when
        it is busy.
      </p>
    )

  const { cells, busiest } = heatmapOf(series)

  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[19rem] border-separate border-spacing-[1px] font-mono text-[10px]">
        <caption className="sr-only">
          Calls by day of the week and hour of the day, in your local time
        </caption>
        <thead>
          <tr>
            <th scope="col">
              <span className="sr-only">Day</span>
            </th>
            {HOURS.map((hour) => (
              <th
                key={hour}
                scope="col"
                className="pb-0.5 font-normal text-muted-foreground"
              >
                {/* Every third hour is *drawn*, because twenty-four labels in
                    19rem is a smear — but every column is still named, or the
                    grid's headers say nothing to a screen reader. */}
                <span className="sr-only">
                  {String(hour).padStart(2, '0')}:00
                </span>
                <span aria-hidden>
                  {hour % 3 === 0 ? String(hour).padStart(2, '0') : ''}
                </span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {cells.map((hours, day) => (
            <tr key={DAY_LABELS[day]}>
              <th
                scope="row"
                className="pr-1 text-right font-normal text-muted-foreground"
              >
                {DAY_LABELS[day]}
              </th>
              {hours.map((calls, hour) => (
                <td
                  key={hour}
                  // The number is the accessible answer; the shade is the
                  // glanceable one. A screen reader gets the cell's own title
                  // rather than a grid of colours it cannot see.
                  title={`${DAY_LABELS[day]} ${String(hour).padStart(2, '0')}:00 — ${calls} calls`}
                  className="h-4 rounded-[1px] bg-muted-foreground"
                  style={{ opacity: shade(calls, busiest) }}
                >
                  <span className="sr-only">{calls}</span>
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

const HOURS = Array.from({ length: 24 }, (_, hour) => hour)

/**
 * How dark a cell is drawn.
 *
 * A floor for anything non-zero, [`barHeights`]'s reason: one Call beside a
 * busiest hour of four hundred is a quarter of a percent of opacity, which is
 * invisible — and a heatmap that shows a busy channel's quiet hours as *nothing
 * at all* is saying something false about them.
 */
function shade(calls: number, busiest: number): number {
  if (calls <= 0) return 0.08
  return Math.max(0.25, calls / Math.max(busiest, 1))
}
