import { describe, expect, it } from 'vitest'

import {
  initialPanelState,
  panelReducer,
  selectExpandedSystems,
  selectPanelSort,
  selectPinned,
  showSystem,
  sortPanel,
  togglePin,
  type PanelState,
} from './panel'

const after = (actions: Parameters<typeof panelReducer>[1][], from = initialPanelState) =>
  actions.reduce(panelReducer, from)

describe('the Talkgroups panel’s own state (#57)', () => {
  it('starts with nothing pinned, nothing said about a System, and catalog order', () => {
    expect(initialPanelState).toEqual({ sort: 'name', pinned: [], expanded: {} })
  })

  it('pins a Talkgroup and unpins it again', () => {
    const pinned = after([togglePin('100:1'), togglePin('200:3')])
    expect(pinned.pinned).toEqual(['100:1', '200:3'])

    expect(after([togglePin('100:1')], pinned).pinned).toEqual(['200:3'])
  })

  /** A Listener's choice about a section is stored as what they *said*, in
   *  both directions — a list of what is shut could not express "leave this
   *  400-row System open", which is the whole point of remembering it. */
  it('remembers a System opened and a System shut', () => {
    const state = after([
      showSystem({ systemRef: 100, shown: true }),
      showSystem({ systemRef: 200, shown: false }),
    ])

    expect(state.expanded).toEqual({ 100: true, 200: false })
  })

  it('changes the sort', () => {
    expect(after([sortPanel('active')]).sort).toBe('active')
  })

  it('reads back off the store', () => {
    const panel: PanelState = { sort: 'active', pinned: ['1:2'], expanded: { 1: true } }

    expect(selectPinned({ panel })).toEqual(['1:2'])
    expect(selectPanelSort({ panel })).toBe('active')
    expect(selectExpandedSystems({ panel })).toEqual({ 1: true })
  })
})
