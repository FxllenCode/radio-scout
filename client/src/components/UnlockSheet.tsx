/**
 * Entering an **Access code** (#68, spec US 52).
 *
 * One sheet over the Talkgroups panel, opened from a locked row or from the
 * count above the list. It does three things and deliberately no more: take the
 * code, hand what the server gives back to the store, and say what happened.
 *
 * The two rules worth knowing are the server's, restated here because this is
 * where a Listener meets them:
 *
 * - **The code goes up in a body, and what comes back rides in local storage.**
 *   A code is a word somebody read out; a grant is 128 minted bits. Only the
 *   second is remembered, which is why "remembers the code per browser" does not
 *   put a shared word in storage. Storing it is the *endpoint's*, because it
 *   has to happen before the refetch it triggers (`store/api`).
 * - **A refusal is keyed on the status**, because the bodies are plain text a
 *   Listener should never be shown and an unlock has exactly three endings —
 *   wrong, expired, and too many tries (`lib/access`).
 */
import { Lock } from 'lucide-react'
import { useId, useState, type FormEvent } from 'react'

import { lockedSentence, unlockFailure } from '@/lib/access'
import { statusOf } from '@/lib/adminError'
import { useUnlockMutation } from '@/store/api'

import { Field, controlClass } from './Field'
import { Sheet } from './Sheet'
import { Button } from './ui/button'

export function UnlockSheet({
  locked,
  onClose,
}: {
  /** How many channels this browser cannot hear — the sentence that makes the
   *  sheet worth opening, and the one it opens with. */
  locked: number
  onClose: () => void
}) {
  const [unlock, { isLoading }] = useUnlockMutation()
  const [code, setCode] = useState('')
  const [refused, setRefused] = useState<string>()
  const field = useId()

  async function submit(event: FormEvent) {
    event.preventDefault()
    // **No guard on an empty code**, because the disabled submit below is the
    // guard: it is the only submit affordance in the form, so Enter on an empty
    // field implicitly submits nothing either. A second check here would be a
    // branch nothing can reach — and an empty unlock is not harmless, since it
    // would spend one of this address's attempts against the lockout.
    setRefused(undefined)
    try {
      // Holding the grant is the endpoint's (`store/api`), because it has to
      // happen before anything refetches and this component is not where that
      // order can be arranged.
      await unlock(code).unwrap()
      // Closed on success and left open on a refusal, so a Listener who
      // mistyped is still looking at the field they mistyped into.
      onClose()
    } catch (error) {
      setRefused(unlockFailure(statusOf(error)))
    }
  }

  return (
    <Sheet title="Unlock channels" onClose={onClose}>
      <form onSubmit={submit} className="flex flex-col gap-4 p-4">
        <p className="font-mono text-xs leading-relaxed text-muted-foreground">
          {lockedSentence(locked)} Enter the access code you were given to hear
          them.
        </p>
        <Field label="Access code" htmlFor={field}>
          <input
            id={field}
            type="password"
            autoComplete="off"
            autoFocus
            value={code}
            onChange={(event) => setCode(event.target.value)}
            className={controlClass}
          />
        </Field>
        {/* `role="alert"` so a refusal is read out: the field keeps its value
            and the only thing that changed is this sentence. */}
        {refused && (
          <p role="alert" className="font-mono text-xs text-destructive">
            {refused}
          </p>
        )}
        <Button type="submit" disabled={isLoading || !code.trim()}>
          <Lock className="size-4" aria-hidden />
          {isLoading ? 'Checking…' : 'Unlock'}
        </Button>
      </form>
    </Sheet>
  )
}
