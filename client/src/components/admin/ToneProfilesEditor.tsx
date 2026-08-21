import { useState } from 'react'

import { FailureNote, Field, controlClass } from '@/components/admin/AdminUi'
import { Button } from '@/components/ui/button'
import {
  DEFAULT_GAP_MAX_MS,
  DEFAULT_TOLERANCE_PCT,
  toneProfileLine,
  unusable,
} from '@/lib/tone'
import {
  useCreateToneProfileMutation,
  useDeleteToneProfileMutation,
  useGetToneProfilesQuery,
  useUpdateToneProfileMutation,
} from '@/store/api'
import type { AdminTalkgroup, ToneStep } from '@/types'

/**
 * **A channel's Tone profiles** (#55, spec US 20) — the stations paged on it,
 * and the tones each is paged with.
 *
 * Three things about this screen are the feature's, not the form's:
 *
 * - **A profile that could never fire is caught here, before it is saved.** The
 *   server refuses the same thing, and this is deliberately a second check
 *   rather than a substitute — [`unusable`](@/lib/tone) is the shared rule, on
 *   `looksPostable`'s terms (#54). It matters more here than in most forms
 *   because the failure it prevents produces no error anywhere: a pager that is
 *   not being watched looks exactly like a pager that has not gone off.
 * - **The sequence is a list an Operator adds to**, not a fixed A/B pair, which
 *   is what lets one form spell a single long group tone, Quick Call II's two,
 *   and an A-B-then-group run of three.
 * - **Disable rather than delete is offered first.** A profile paging on
 *   somebody else's tones is something an Operator wants stopped *now* and
 *   worked out later, and deleting it would lose the frequencies they measured.
 *
 * Deleting one keeps the pages it already caught: each Call snapshotted the
 * label when it fired, so the Archive keeps saying who was paged.
 */
export function ToneProfilesEditor({ row }: { row: AdminTalkgroup }) {
  const profiles = useGetToneProfilesQuery(row.id)
  const [remove, removing] = useDeleteToneProfileMutation()
  const [update] = useUpdateToneProfileMutation()

  const held = profiles.data?.results ?? []

  return (
    <section
      aria-label="Tone profiles"
      className="mt-2 flex flex-col gap-2 border-t border-border pt-2"
    >
      <p className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
        Paged on this channel
      </p>
      {held.length === 0 ? (
        <p className="font-mono text-xs text-muted-foreground">
          {profiles.isFetching
            ? 'Reading…'
            : 'Nothing is being listened for on this channel.'}
        </p>
      ) : (
        <ul aria-label="Profiles" className="flex flex-col gap-1">
          {held.map((profile) => (
            <li
              key={profile.id}
              className="flex flex-wrap items-center justify-between gap-2 font-mono text-xs"
            >
              <span>
                <span className="font-semibold">{profile.label}</span>
                {` — ${toneProfileLine(profile)}`}
              </span>
              <span className="flex gap-1">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    update({
                      id: profile.id,
                      patch: { disabled: !profile.disabled },
                    })
                  }
                >
                  {profile.disabled ? 'Enable' : 'Disable'}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => remove(profile.id)}
                >
                  {`Delete ${profile.label}`}
                </Button>
              </span>
            </li>
          ))}
        </ul>
      )}
      {removing.error != null && <FailureNote error={removing.error} />}
      <NewProfileForm talkgroupId={row.id} />
    </section>
  )
}

/** The form that adds one. Its own component so the tone list it accumulates is
 *  its own state, cleared by a successful save rather than by the parent
 *  remembering to. */
function NewProfileForm({ talkgroupId }: { talkgroupId: number }) {
  const [create, creating] = useCreateToneProfileMutation()
  const [label, setLabel] = useState('')
  const [steps, setSteps] = useState<ToneStep[]>([])
  const [hz, setHz] = useState('')
  const [seconds, setSeconds] = useState('1.0')
  const [tolerance, setTolerance] = useState(String(DEFAULT_TOLERANCE_PCT))
  const [gap, setGap] = useState(String(DEFAULT_GAP_MAX_MS))

  const tolerancePct = Number(tolerance)
  const gapMaxMs = Number(gap)
  // Only once there is something to judge: telling an Operator their empty form
  // needs at least one tone before they have typed anything is noise.
  const refused =
    steps.length === 0 ? null : unusable(steps, tolerancePct, gapMaxMs)

  const addStep = () => {
    const minMs = Math.round(Number(seconds) * 1000)
    setSteps([...steps, { hz: Number(hz), minMs }])
    setHz('')
  }

  const save = async () => {
    await create({
      talkgroupId,
      body: { label, steps, tolerancePct, gapMaxMs },
    }).unwrap()
    setLabel('')
    setSteps([])
  }

  return (
    <form
      aria-label="Add a tone profile"
      className="flex flex-col gap-2 border-t border-border pt-2"
      onSubmit={(event) => {
        event.preventDefault()
        void save().catch(() => {})
      }}
    >
      <Field label="Station" htmlFor="tone-label">
        <input
          id="tone-label"
          className={controlClass}
          placeholder="Station 12"
          value={label}
          onChange={(event) => setLabel(event.target.value)}
        />
      </Field>

      {steps.length > 0 && (
        <ol
          aria-label="Sequence"
          className="flex flex-col gap-1 font-mono text-xs"
        >
          {steps.map((step, index) => (
            <li
              key={`${step.hz}-${step.minMs}-${index}`}
              className="flex items-center justify-between gap-2"
            >
              <span>{`${index + 1}. ${step.hz} Hz for ${(step.minMs / 1000).toFixed(1)}s`}</span>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() =>
                  setSteps(steps.filter((_, at) => at !== index))
                }
              >
                {`Remove tone ${index + 1}`}
              </Button>
            </li>
          ))}
        </ol>
      )}

      <div className="flex flex-wrap items-end gap-2">
        <Field label="Tone (Hz)" htmlFor="tone-hz">
          <input
            id="tone-hz"
            type="number"
            step="0.1"
            inputMode="decimal"
            className={controlClass}
            placeholder="1122.5"
            value={hz}
            onChange={(event) => setHz(event.target.value)}
          />
        </Field>
        <Field label="Held for (s)" htmlFor="tone-seconds">
          <input
            id="tone-seconds"
            type="number"
            step="0.1"
            inputMode="decimal"
            className={controlClass}
            value={seconds}
            onChange={(event) => setSeconds(event.target.value)}
          />
        </Field>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={hz === ''}
          onClick={addStep}
        >
          Add tone
        </Button>
      </div>

      <div className="flex flex-wrap items-end gap-2">
        <Field label="Tolerance (%)" htmlFor="tone-tolerance">
          <input
            id="tone-tolerance"
            type="number"
            step="0.1"
            inputMode="decimal"
            className={controlClass}
            value={tolerance}
            onChange={(event) => setTolerance(event.target.value)}
          />
        </Field>
        <Field label="Max gap (ms)" htmlFor="tone-gap">
          <input
            id="tone-gap"
            type="number"
            inputMode="numeric"
            className={controlClass}
            value={gap}
            onChange={(event) => setGap(event.target.value)}
          />
        </Field>
      </div>

      {/* Said before the form can be submitted, not after — the whole reason
          the rule is duplicated in the browser. */}
      {refused != null && (
        <p role="alert" className="font-mono text-xs text-destructive">
          {refused}
        </p>
      )}
      {creating.error != null && <FailureNote error={creating.error} />}

      <Button
        type="submit"
        size="sm"
        disabled={label.trim() === '' || steps.length === 0 || refused != null}
      >
        Add profile
      </Button>
    </form>
  )
}
