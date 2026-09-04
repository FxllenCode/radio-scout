import { describe, expect, it } from 'vitest'

import type { Call } from '@/types'

import {
  forgetStar,
  initialStarsState,
  markStarred,
  selectStarred,
  starsReducer,
} from './stars'

const call = (id: number, starred?: boolean): Call =>
  ({ id, systemRef: 1, talkgroupRef: 2, ...(starred === undefined ? {} : { starred }) }) as Call

const state = (marks: Record<number, boolean>) => ({ stars: { marks } })

describe('stars', () => {
  it('says what the server said about a Call nobody has touched this session', () => {
    expect(selectStarred(state({}), call(1))).toBe(false)
    expect(selectStarred(state({}), call(1, true))).toBe(true)
  })

  it('lets what this session did outrank what the page was fetched with', () => {
    // The reason the override exists: a Call lives in two places at once — the
    // archive's RTK Query cache and the live slice — and only one of them can
    // be refetched. Both read through here.
    expect(selectStarred(state({ 1: true }), call(1))).toBe(true)
    expect(selectStarred(state({ 1: false }), call(1, true))).toBe(false)
  })

  it('remembers a mark and can forget it again', () => {
    const marked = starsReducer(
      initialStarsState,
      markStarred({ id: 7, starred: true }),
    )
    expect(marked.marks).toEqual({ 7: true })

    // Forgetting is what a *refused* star does, and it deliberately does not
    // write `false`: it falls back to whatever the server last said, which on
    // a failed un-star is still `true`.
    expect(starsReducer(marked, forgetStar(7)).marks).toEqual({})
  })

  it('keeps one mark per Call however often it is toggled', () => {
    let marks = initialStarsState
    for (const starred of [true, false, true]) {
      marks = starsReducer(marks, markStarred({ id: 7, starred }))
    }
    expect(marks.marks).toEqual({ 7: true })
  })
})
