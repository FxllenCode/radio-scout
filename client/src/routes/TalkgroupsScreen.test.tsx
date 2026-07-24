import { render, screen } from '@testing-library/react'
import { axe } from 'vitest-axe'
import { describe, expect, it } from 'vitest'

import { TalkgroupsScreen } from './TalkgroupsScreen'

describe('TalkgroupsScreen', () => {
  it('renders the empty-state placeholder', () => {
    render(<TalkgroupsScreen />)
    expect(
      screen.getByRole('heading', { name: 'Talkgroups' }),
    ).toBeInTheDocument()
    expect(screen.getByText('No systems yet.')).toBeInTheDocument()
  })

  it('has no accessibility violations', async () => {
    const { container } = render(<TalkgroupsScreen />)
    expect(await axe(container)).toHaveNoViolations()
  })
})
