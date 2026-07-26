import { act, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { axe } from 'vitest-axe'

import {
  beIosSafari,
  offerInstall,
  type FakeInstallPromptEvent,
} from '@/test/install'
import { renderWithProviders } from '@/test/utils'

import { InstallBanner } from './InstallBanner'

// A browser may have no storage at all (this one doesn't) — clear whatever is
// there, so a dismissal in one test can't decide the next one.
beforeEach(() => globalThis.localStorage?.clear())

describe('InstallBanner', () => {
  it('offers to install once the browser volunteers a prompt', async () => {
    renderWithProviders(<InstallBanner />)
    expect(screen.queryByText(/install radio-scout/i)).not.toBeInTheDocument()

    let event!: FakeInstallPromptEvent
    act(() => {
      event = offerInstall()
    })

    await userEvent.click(screen.getByRole('button', { name: /^install$/i }))

    expect(event.prompted).toBe(1)
  })

  // iOS has no install dialog to offer — and it is the platform that most needs
  // installing, since standalone is what background audio and push require.
  it('shows the Share-sheet steps where there is no install dialog', () => {
    beIosSafari()

    renderWithProviders(<InstallBanner />)

    expect(screen.getByText(/add to home screen/i)).toBeInTheDocument()
    expect(
      screen.queryByRole('button', { name: /^install$/i }),
    ).not.toBeInTheDocument()
  })

  it('goes away when dismissed, and does not come back', async () => {
    renderWithProviders(<InstallBanner />)
    act(() => void offerInstall())

    await userEvent.click(
      screen.getByRole('button', { name: /dismiss install/i }),
    )

    expect(screen.queryByText(/install radio-scout/i)).not.toBeInTheDocument()
    act(() => void offerInstall())
    expect(screen.queryByText(/install radio-scout/i)).not.toBeInTheDocument()
  })

  it('is accessible', async () => {
    const { container } = renderWithProviders(<InstallBanner />)
    act(() => void offerInstall())

    await expect(axe(container)).resolves.toHaveNoViolations()
  })
})
