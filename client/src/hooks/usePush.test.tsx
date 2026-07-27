import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { usePush } from './usePush'

function Probe() {
  const { push, state } = usePush()
  return <span>{`${state}:${push === null}`}</span>
}

describe('usePush outside a provider', () => {
  // Every screen is rendered by the shell, which provides one — but a component
  // rendered on its own (a test, a future storybook) must not crash, and
  // "no handle" is indistinguishable from "this browser cannot", which is
  // exactly what the switch already knows how to render.
  it('reports unsupported rather than throwing', () => {
    render(<Probe />)

    expect(screen.getByText('unsupported:true')).toBeInTheDocument()
  })
})
