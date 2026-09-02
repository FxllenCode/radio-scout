import { useEffect, useState, type ReactNode } from 'react'

import { dateTimeLocalToMs, msToDateTimeLocal } from '@/lib/archive'

/** How every control on a filter form is drawn, so a form added later matches
 *  the ones beside it without copying a class list. */
export const controlClass =
  'w-full rounded-md border border-border bg-background px-2 py-1.5 font-mono text-xs text-foreground'

/** A labelled control. */
export function Field({
  label,
  htmlFor,
  children,
}: {
  label: string
  htmlFor: string
  children: ReactNode
}) {
  return (
    <div className="flex flex-col gap-1">
      <label
        htmlFor={htmlFor}
        className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
      >
        {label}
      </label>
      {children}
    </div>
  )
}

/**
 * A date bound as a control (#61).
 *
 * Controlled, because a bound can arrive from somewhere other than this input —
 * a preset, or a link — and an uncontrolled box would go on showing whatever
 * was last typed while the search behind it said something else.
 *
 * It keeps the *text* rather than deriving it, and re-derives only when the
 * bound it is shown is not the one the text already means. Both halves matter:
 * without the local text, a half-typed date would be parsed, rejected and wiped
 * on every keystroke; without the guard, a partially-typed date that happens to
 * parse (`2026-07-25` is a valid instant — at UTC midnight) would be rewritten
 * under the Listener's cursor mid-word.
 *
 * Shared rather than copied (#63): the **DVR** picks a range too, and a second
 * copy of that rule would be a second chance to get one of its two halves
 * wrong — in a way that only shows up as a date box fighting whoever types
 * into it.
 */
export function DateField({
  label,
  id,
  ms,
  onChange,
}: {
  label: string
  id: string
  ms: number | undefined
  onChange: (ms: number | undefined) => void
}) {
  const [text, setText] = useState(() => msToDateTimeLocal(ms))
  useEffect(() => {
    setText((typed) =>
      dateTimeLocalToMs(typed) === ms ? typed : msToDateTimeLocal(ms),
    )
  }, [ms])

  return (
    <Field label={label} htmlFor={id}>
      <input
        id={id}
        type="datetime-local"
        className={controlClass}
        value={text}
        onChange={(event) => {
          setText(event.target.value)
          onChange(dateTimeLocalToMs(event.target.value))
        }}
      />
    </Field>
  )
}
