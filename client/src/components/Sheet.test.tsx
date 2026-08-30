import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { axe } from 'vitest-axe'

import { Sheet } from './Sheet'

const open = (onClose = vi.fn()) => {
  const view = render(
    <Sheet title="Waiting" onClose={onClose}>
      <p>three calls</p>
    </Sheet>,
  )
  return { ...view, onClose }
}

describe('a sheet (#58)', () => {
  it('is a dialog named by its title', () => {
    open()

    expect(screen.getByRole('dialog', { name: 'Waiting' })).toBeInTheDocument()
    expect(screen.getByText('three calls')).toBeInTheDocument()
  })

  /** Where a screen reader lands, and what makes Escape work without the
   *  Listener having tabbed anywhere first. */
  it('takes focus when it opens', () => {
    open()

    expect(screen.getByRole('dialog')).toHaveFocus()
  })

  /**
   * Focus is on the *panel*, deliberately not on its first control: a sheet
   * that opened with *Drop* focused would be one Enter away from discarding a
   * Call the Listener opened the sheet to look at.
   */
  it('does not focus a control inside it', () => {
    open()

    expect(screen.getByRole('button', { name: 'Close' })).not.toHaveFocus()
  })

  it('closes on Escape', async () => {
    const { onClose } = open()

    await userEvent.keyboard('{Escape}')

    expect(onClose).toHaveBeenCalledOnce()
  })

  /** Only Escape. A sheet that closed on any key would take itself down under
   *  a Listener tabbing through its rows. */
  it('stays open on any other key', async () => {
    const { onClose } = open()

    await userEvent.keyboard('{Enter}{ArrowDown}x')

    expect(onClose).not.toHaveBeenCalled()
  })

  it('closes on its Close control', async () => {
    const { onClose } = open()

    await userEvent.click(screen.getByRole('button', { name: 'Close' }))

    expect(onClose).toHaveBeenCalledOnce()
  })

  /** Tapping away is how a sheet is dismissed on a phone. */
  it('closes when the backdrop is tapped', async () => {
    const { onClose, container } = open()

    await userEvent.click(container.querySelector('[aria-hidden]')!)

    expect(onClose).toHaveBeenCalledOnce()
  })

  it('has no accessibility violations', async () => {
    const { container } = open()

    expect(await axe(container)).toHaveNoViolations()
  })
})
