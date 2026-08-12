import { useState, type ReactNode } from 'react'

import { Button } from '@/components/ui/button'
import { signInMessage } from '@/lib/adminError'
import { curateFailure, refusedForCalls } from '@/lib/curateError'
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
