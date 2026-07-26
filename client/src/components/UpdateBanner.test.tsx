import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { useAppUpdate } from '@/hooks/useAppUpdate'
import { SKIP_WAITING } from '@/lib/serviceWorker'
import { deploy, installServiceWorker, registered } from '@/test/serviceWorker'
import { renderWithProviders } from '@/test/utils'

import { UpdateBanner } from './UpdateBanner'

/** What the hook is *for*: something that shows the banner once a version is
 *  waiting. Which banner wins the shell's one docked slot when an install
 *  offer competes is the shell's own decision, and is tested there. */
function Updates() {
  const update = useAppUpdate()
  return update.ready ? <UpdateBanner apply={update.apply} /> : null
}

describe('UpdateBanner', () => {
  it('stays out of the way until there is a new version', async () => {
    const container = installServiceWorker()

    renderWithProviders(<Updates />)
    await registered(container)

    expect(screen.queryByText(/update/i)).not.toBeInTheDocument()
  })

  // The listener decides when to take it: a reload of its own accord would cut
  // off whatever is playing (ADR-0005), which is the whole reason we wait.
  it('offers the new version, and hands over only when asked', async () => {
    const container = installServiceWorker()
    renderWithProviders(<Updates />)

    const waiting = await deploy(container)

    expect(waiting.posted).toEqual([])

    await userEvent.click(screen.getByRole('button', { name: /reload/i }))

    expect(waiting.posted).toEqual([SKIP_WAITING])
  })
})
