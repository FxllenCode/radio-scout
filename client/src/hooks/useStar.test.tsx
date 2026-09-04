import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { renderWithProviders } from '@/test/utils'
import type { Call } from '@/types'

import { useStar } from './useStar'

const call: Call = { id: 1, systemRef: 11, talkgroupRef: 54241 }

function Probe({ subject }: { subject: Call | null }) {
  const { starred, toggle } = useStar(subject)
  return (
    <button type="button" onClick={toggle}>
      {starred ? 'starred' : 'not starred'}
    </button>
  )
}

describe('useStar (#66)', () => {
  it('stars the Call it was given', async () => {
    const user = userEvent.setup()
    renderWithProviders(<Probe subject={call} />)

    await user.click(screen.getByRole('button'))

    expect(await screen.findByRole('button', { name: 'starred' })).toBeInTheDocument()
  })

  /** The Live display before the first Call, with the feed off, and in playback
   *  mode: the control is drawn — a scanner's face does not lose buttons — and
   *  there is nothing under it to keep. */
  it('does nothing with no Call under it', async () => {
    const user = userEvent.setup()
    renderWithProviders(<Probe subject={null} />)

    await user.click(screen.getByRole('button'))

    expect(screen.getByRole('button', { name: 'not starred' })).toBeInTheDocument()
  })
})
