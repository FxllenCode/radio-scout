/**
 * ## Design notes (moved verbatim from CLAUDE.md, #110)
 * **The admin section is one gate and one CSRF decision (#49).** `components/admin/AdminUi.tsx`'s `AdminGate` is the password gate all seven operator screens sit behind — the client's counterpart to the server's prefix layer, and what the Logs screen's own inline sign-in became when there stopped being one screen behind it. The CSRF token is attached in **`prepareHeaders`**, on mutations only, read from the session already in the RTK Query cache: a mutation added later inherits it instead of remembering a header, which is exactly the class of omission #18 had to be shipped around on the server. Three more things worth knowing. **A refusal is rendered from the server's own sentence** (`lib/curateError.ts`) rather than re-derived — the server is the side that knows the LED palette and how many Calls a delete would take — and `refusedForCalls` gates the offer to force on the *slug*, never the status, because a name collision is also a 409 and a "delete it anyway" button there would do nothing. **A form's buttons need `type="button"`**: `Button` renders a bare `<button>`, so "Deselect" and "Clear their tags" were submitting the bulk action until a test caught it. And **changing a filter drops the selection**, because a bulk action over rows that scrolled out of the answer is the one thing multi-select must never do.
 */
import { useState, type ReactNode } from 'react'

import { Button } from '@/components/ui/button'
import { signInMessage } from '@/lib/adminError'
import { curateFailure, refusedForCalls } from '@/lib/curateError'
import { keepWindow, type KeepMode } from '@/lib/curate'
import {
  useAdminLoginMutation,
  useAdminLogoutMutation,
  useGetAdminSessionQuery,
} from '@/store/api'

/** The look every admin control shares — one string, so a form added by a later
 *  ticket does not invent a second one. */
export const controlClass =
  'w-full rounded-md border border-border bg-background px-2 py-1.5 font-mono text-xs text-foreground'

/**
 * The password gate every admin screen sits behind (#19, #49).
 *
 * One component rather than one per screen, for the reason the server mounts its
 * guard as a prefix layer: a screen added beside the others inherits the gate
 * instead of remembering it. Until #49 there was one screen behind it (Logs) and
 * the sign-in lived inside that screen; there are now seven.
 *
 * The session is a **server-side** record behind an httpOnly cookie (ADR-0008),
 * so this asks the server rather than reading anything local — which is also
 * what makes a reloaded page work: it still holds the cookie, and asks for the
 * CSRF token it no longer has.
 */
export function AdminGate({ children }: { children: ReactNode }) {
  const session = useGetAdminSessionQuery()

  if (session.isLoading)
    return <Placeholder>Checking your session…</Placeholder>
  if (!session.isSuccess) return <SignIn />
  return <>{children}</>
}

/** Sign out — server-side, because ADR-0008 chose session state over a JWT
 *  precisely so revocation is real. Rendered in a screen's `status` slot. */
export function SignOutButton() {
  const [logout] = useAdminLogoutMutation()

  return (
    <Button variant="outline" size="sm" onClick={() => logout()}>
      Sign out
    </Button>
  )
}

/** The password form. */
function SignIn() {
  const [password, setPassword] = useState('')
  const [login, attempt] = useAdminLoginMutation()

  return (
    <form
      className="mt-3 flex flex-col gap-3 rounded-xl border border-border bg-card px-4 py-5"
      onSubmit={(event) => {
        event.preventDefault()
        login(password)
      }}
    >
      <p className="font-mono text-xs text-muted-foreground">
        This is the operator surface: it configures the instance and shows what
        it has been doing.
      </p>
      <div className="flex flex-col gap-1">
        <label
          htmlFor="admin-password"
          className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
        >
          Admin password
        </label>
        <input
          id="admin-password"
          type="password"
          autoComplete="current-password"
          className={controlClass}
          value={password}
          onChange={(event) => setPassword(event.target.value)}
        />
      </div>
      {attempt.isError && (
        <p role="alert" className="font-mono text-xs text-red-400">
          {signInMessage(attempt.error)}
        </p>
      )}
      <Button type="submit" size="sm" disabled={attempt.isLoading}>
        {attempt.isLoading ? 'Signing in…' : 'Sign in'}
      </Button>
    </form>
  )
}

/** The empty / loading / failed states a list stands in for. */
export function Placeholder({
  role,
  children,
}: {
  role?: 'alert'
  children: ReactNode
}) {
  return (
    <p
      role={role}
      className="mt-3 rounded-xl border border-border bg-card px-6 py-8 text-center font-mono text-sm text-muted-foreground"
    >
      {children}
    </p>
  )
}

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
 * What a refused write says, **beside the form that caused it**.
 *
 * The acceptance criterion is that validation errors surface inline and nothing
 * is silently ignored, so this renders the server's own sentence — the side that
 * knows the LED palette, the collision that happened, and how many Calls a
 * delete would have taken. rdio's config `PUT` coerces what it can and answers
 * `400` with nothing, which is how an Operator ends up believing they changed a
 * setting they did not.
 *
 * A delete refused for Calls is the one refusal with an answer, so it offers
 * one: `onForce` renders the button that asks again on purpose.
 */
export function FailureNote({
  error,
  onForce,
}: {
  error: unknown
  onForce?: () => void
}) {
  const failure = curateFailure(error)

  return (
    <div
      role="alert"
      className="flex flex-col gap-2 font-mono text-xs text-red-400"
    >
      <span>{failure.detail}</span>
      {onForce && refusedForCalls(failure) && (
        <Button variant="outline" size="sm" onClick={onForce}>
          Delete it and its {failure.calls} calls
        </Button>
      )}
    </div>
  )
}

/** A row's expandable editor. Inline rather than a modal: an Operator working
 *  down a county's channels is editing one row after another, and a dialog per
 *  row is a dialog dismissed a hundred times. */
export function RowCard({
  title,
  subtitle,
  children,
  actions,
}: {
  title: ReactNode
  subtitle?: ReactNode
  children?: ReactNode
  actions?: ReactNode
}) {
  return (
    <li className="flex flex-col gap-2 px-3 py-2.5">
      <div className="flex items-baseline justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate font-mono text-sm">{title}</p>
          {subtitle && (
            <p className="truncate font-mono text-[11px] text-muted-foreground">
              {subtitle}
            </p>
          )}
        </div>
        {actions && <div className="flex shrink-0 gap-2">{actions}</div>}
      </div>
      {children}
    </li>
  )
}

/** The card a list of rows sits in. */
export function RowList({
  label,
  children,
}: {
  label: string
  children: ReactNode
}) {
  return (
    <ul
      aria-label={label}
      className="mt-3 divide-y divide-border rounded-xl border border-border bg-card"
    >
      {children}
    </ul>
  )
}

/**
 * How long a System's or a channel's Calls are kept (#69, spec US 53).
 *
 * **One control for both entities**, the `inherit` select's reason one setting
 * along: the System form and the Talkgroup form are asking the identical
 * question, and two hand-written copies would be two chances for one of them to
 * spell *forever* differently.
 *
 * Three options and a number box rather than a number box alone, because the
 * wire spends `0` on **keep for good** — the reading `[retention] days` already
 * has — and a box labelled "days to keep" where `0` means *never delete* is a
 * trap an Operator falls into exactly once, irreversibly. [`keepMode`] and
 * [`keepWindow`] are the two halves of the translation, and they are pure so the
 * rule is tested without a DOM.
 *
 * The days box stays mounted and merely disabled under the other two modes, so
 * switching to *forever* and back does not lose the number that was typed.
 */
export function RetentionField({
  id,
  mode,
  days,
  onMode,
  onDays,
  inheritsFrom,
}: {
  /** Unique per mounted form, so two rows open at once do not share input ids. */
  id: string
  mode: KeepMode
  /** The raw text of the days box — text, not a number, for `ScopeEditor`'s
   *  reason: re-rendering a field from its parsed value fights the typing. */
  days: string
  onMode: (mode: KeepMode) => void
  onDays: (days: string) => void
  /** What *inherit* follows here — the instance for a System, the System for a
   *  channel. A select whose first option said only "inherit" would leave an
   *  Operator guessing which of the two levels above it meant. */
  inheritsFrom: string
}) {
  return (
    <div className="grid grid-cols-[1fr_6rem] gap-2">
      <Field label="Keep calls" htmlFor={`${id}-keep`}>
        <select
          id={`${id}-keep`}
          className={controlClass}
          value={mode}
          onChange={(event) => onMode(event.target.value as KeepMode)}
        >
          <option value="inherit">{inheritsFrom}</option>
          <option value="days">For a number of days</option>
          <option value="forever">For good — never delete them</option>
        </select>
      </Field>
      <Field label="Days" htmlFor={`${id}-keep-days`}>
        <input
          id={`${id}-keep-days`}
          className={controlClass}
          inputMode="numeric"
          disabled={mode !== 'days'}
          value={days}
          onChange={(event) => onDays(event.target.value)}
        />
      </Field>
      {keepWindow(mode, days) === undefined && (
        // The blacklist field's rule: refused under the input, before the
        // request. It lives in the control rather than in the two forms that
        // mount it, so neither has to remember the sentence — they owe only the
        // disabled button, which is the one thing they own.
        <p role="alert" className="col-span-2 font-mono text-xs text-red-400">
          Give a whole number of days, or choose one of the other two options.
        </p>
      )}
    </div>
  )
}
