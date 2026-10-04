import { describe, expect, it } from 'vitest'

import type { AdminDirwatch } from '@/types'

import {
  FORMATS,
  fieldsFor,
  formatChoice,
  healthLine,
  wholeNumber,
} from './dirwatch'

/** A healthy watch, for the health line's cases to vary one thing from. */
function watch(row: Partial<AdminDirwatch> = {}): AdminDirwatch {
  return {
    id: 1,
    label: null,
    directory: '/srv/trunk-recorder',
    format: 'trunk-recorder',
    delayMs: 2000,
    deleteAfter: false,
    poll: false,
    disabled: false,
    createdAtMs: 1_700_000_000_000,
    health: { status: 'watching', ingested: 0, refused: 0 },
    ...row,
  }
}

describe('a Dirwatch format, as the form offers it', () => {
  it('lists every format the server reads, Trunk Recorder first', () => {
    expect(FORMATS.map((format) => format.value)).toEqual([
      'trunk-recorder',
      'sdrtrunk',
      'dsdplus',
      'mask',
    ])
  })

  it('finds a format by its value, and falls back for one it does not list', () => {
    expect(formatChoice('dsdplus').label).toBe('DSDPlus Fast Lane')
    expect(formatChoice('nonsense' as never).value).toBe('trunk-recorder')
  })

  /** A field the format ignores is not drawn. */
  it.each([
    ['trunk-recorder', { mask: false, talkgroup: false }],
    ['sdrtrunk', { mask: false, talkgroup: false }],
    ['dsdplus', { mask: false, talkgroup: true }],
    ['mask', { mask: true, talkgroup: true }],
  ] as const)('shows %s only the fields it reads', (format, fields) => {
    expect(fieldsFor(format)).toEqual(fields)
  })
})

describe("a watch's health line", () => {
  it('says what it is and that it is watching', () => {
    expect(healthLine(watch())).toBe('Trunk Recorder · watching · 0 ingested')
  })

  it('says when it last ingested, and that it deletes as it goes', () => {
    const line = healthLine(
      watch({
        deleteAfter: true,
        health: { status: 'watching', ingested: 3, refused: 0, lastIngestMs: 1_700_000_000_000 },
      }),
    )

    expect(line).toMatch(/deletes as it goes · 3 ingested, last \d{4}-/)
  })

  it('says how many files were refused, and the last one', () => {
    expect(
      healthLine(
        watch({
          health: { status: 'watching', ingested: 0, refused: 2, lastRefusal: 'no-match a.wav' },
        }),
      ),
    ).toContain('2 refused · no-match a.wav')
    expect(
      healthLine(watch({ health: { status: 'watching', ingested: 0, refused: 1 } })),
    ).toMatch(/1 refused$/)
  })

  /** A watch that is not running says the one thing to fix. */
  it.each([
    ['starting', 'starting'],
    ['disabled', 'disabled'],
    ['outside-roots', 'outside [dirwatch] roots'],
    ['no-such-directory', 'the folder is gone'],
    ['cannot-watch', 'refused to watch it'],
    ['unreadable', 'cannot read its settings'],
  ] as const)('says what %s means', (status, says) => {
    expect(
      healthLine(watch({ health: { status, ingested: 0, refused: 0 } })),
    ).toContain(says)
  })
})

describe('a number typed into the form', () => {
  it.each([
    ['', undefined],
    ['  ', undefined],
    ['11', 11],
    [' 2000 ', 2000],
    ['-1', null],
    ['1.5', null],
    ['eleven', null],
  ])('reads %j as %s', (text, number) => {
    expect(wholeNumber(text)).toBe(number)
  })
})
