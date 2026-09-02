import {
  AdminGate,
  FailureNote,
  Placeholder,
  RowCard,
  RowList,
  SignOutButton,
} from '@/components/admin/AdminUi'
import { Screen } from '@/components/layout/Screen'
import { Button } from '@/components/ui/button'
import { useState } from 'react'

import { useAdminSession } from '@/hooks/useAdminSession'
import { formatCallTime, pageSummary } from '@/lib/archive'
import { callCategory, systemName, talkgroupName } from '@/lib/call'
import { useDeleteShareLinkMutation, useGetShareLinksQuery } from '@/store/api'

/** How many links a screenful holds. */
const PAGE_SIZE = 50

/**
 * Settings → Admin → Share links (#64, spec US 32) — what this Instance is
 * currently sharing, and the one thing an Operator does about it.
 *
 * **A listing and a revoke, and nothing else.** Minting is the **Listener's**,
 * so there is no create; the only field worth editing is the expiry, and a link
 * that has run out is re-shared rather than extended.
 *
 * **The token is not on this screen.** The link *is* the credential, so the
 * server never returns it — the `WebhooksScreen` rule, one row along. What is
 * shown instead is the **Call** the link opens, which is what an Operator is
 * actually asking ("what is being shared from my instance?") and is already one
 * tap away in the archive.
 *
 * Revoking deletes the row, so that URL is dead for good — a token is 128 random
 * bits and is never reissued. A Listener may share the Call again afterwards and
 * gets a *new* link, which is exactly what revoking a leaked URL should mean.
 *
 * **Paged, not capped.** Minting takes no credential, so the number of rows is
 * the number of Calls rather than the number of times somebody pressed a button
 * — and a screen that showed the newest fifty and stopped would leave every
 * older link invisible *and* unrevocable, which is the one thing it is for.
 */
export function SharesScreen() {
  const signedIn = useAdminSession()
  const [offset, setOffset] = useState(0)
  const listing = useGetShareLinksQuery(
    { limit: PAGE_SIZE, offset },
    { skip: !signedIn },
  )
  const [revoke, revoking] = useDeleteShareLinkMutation()

  const rows = listing.data?.results ?? []

  return (
    <Screen title="Share links" status={<SignOutButton />}>
      <AdminGate>
        <p className="mt-3 px-1 font-mono text-[11px] text-muted-foreground">
          Public links listeners have minted for a single call. Each one plays
          that call and reaches nothing else here, and stops working on its own.
          Revoking kills the link that was handed out — sharing the call again
          mints a different one.
        </p>

        {rows.length === 0 ? (
          <Placeholder>
            {listing.isLoading ? 'Loading…' : 'Nothing is being shared.'}
          </Placeholder>
        ) : (
          <RowList label="Share links">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                title={talkgroupName(row.call)}
                subtitle={[
                  systemName(row.call),
                  callCategory(row.call),
                  row.call.timestamp !== undefined &&
                    formatCallTime(row.call.timestamp),
                  row.expired
                    ? 'expired'
                    : `until ${formatCallTime(row.expiresAtMs)}`,
                ]
                  .filter(Boolean)
                  .join(' · ')}
                actions={
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => revoke(row.id)}
                  >
                    Revoke
                  </Button>
                }
              />
            ))}
          </RowList>
        )}
        {revoking.error != null && <FailureNote error={revoking.error} />}

        <div className="mt-4 flex items-center justify-between gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={offset === 0}
            onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
          >
            Previous
          </Button>
          <p
            aria-live="polite"
            className="font-mono text-xs text-muted-foreground"
          >
            {pageSummary(offset, rows.length, listing.data?.count ?? 0, 'links')}
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={!listing.data?.hasMore}
            onClick={() => setOffset(offset + PAGE_SIZE)}
          >
            Next
          </Button>
        </div>
      </AdminGate>
    </Screen>
  )
}
