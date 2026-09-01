import { describe, expect, it, vi } from 'vitest'

import { linkTo, shareLink, shareNotice, type Sharer } from './share'

describe('the link a listener sends', () => {
  it('is absolute, because a path is not something you can paste anywhere', () => {
    expect(linkTo('/search', 'tag=Fire', 'http://scanner.local:3000')).toBe(
      'http://scanner.local:3000/search?tag=Fire',
    )
  })

  it('is the bare view when there is no query to carry', () => {
    expect(linkTo('/talkgroups', '', 'http://scanner.local:3000')).toBe(
      'http://scanner.local:3000/talkgroups',
    )
  })
})

const sharer = (over: Sharer = {}): Sharer => over

describe('handing a link to the platform', () => {
  it('offers the share sheet when there is one, which is how a link becomes a text message', async () => {
    const share = vi.fn().mockResolvedValue(undefined)
    const writeText = vi.fn().mockResolvedValue(undefined)
    await expect(
      shareLink('http://scanner.local/search?tag=Fire', 'Fire calls', {
        share,
        clipboard: { writeText },
      }),
    ).resolves.toBe('shared')
    expect(share).toHaveBeenCalledWith({
      title: 'Fire calls',
      url: 'http://scanner.local/search?tag=Fire',
    })
    expect(writeText).not.toHaveBeenCalled()
  })

  it('copies when the platform has no share sheet — every desktop browser', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    await expect(
      shareLink('http://scanner.local/search', 'Search', sharer({ clipboard: { writeText } })),
    ).resolves.toBe('copied')
    expect(writeText).toHaveBeenCalledWith('http://scanner.local/search')
  })

  /** Cancelling a share sheet is a decision, not a failure: reporting "could
   *  not share" for it would tell a Listener something went wrong when they are
   *  the thing that stopped it. */
  it('says nothing happened when the listener waves the sheet away', async () => {
    const share = vi.fn().mockRejectedValue(
      Object.assign(new Error('share canceled'), { name: 'AbortError' }),
    )
    const writeText = vi.fn().mockResolvedValue(undefined)
    await expect(
      shareLink('http://scanner.local/search', 'Search', { share, clipboard: { writeText } }),
    ).resolves.toBe('dismissed')
    // And emphatically does not then copy it: the Listener said no.
    expect(writeText).not.toHaveBeenCalled()
  })

  it('falls back to the clipboard when the sheet refuses for any other reason', async () => {
    const share = vi.fn().mockRejectedValue(new Error('not allowed'))
    const writeText = vi.fn().mockResolvedValue(undefined)
    await expect(
      shareLink('http://scanner.local/search', 'Search', { share, clipboard: { writeText } }),
    ).resolves.toBe('copied')
    expect(writeText).toHaveBeenCalled()
  })

  it.each([
    ['there is neither', sharer()],
    ['the clipboard is denied', sharer({ clipboard: { writeText: () => Promise.reject(new Error('denied')) } })],
  ])('says so when %s', async (_what, platform) => {
    await expect(shareLink('http://scanner.local/search', 'Search', platform)).resolves.toBe(
      'failed',
    )
  })
})

/** What the Listener is told afterwards. Here rather than in the screen so the
 *  four outcomes are covered once, in a table, instead of four renders each. */
describe('what a share is worth saying', () => {
  it.each([
    ['copied', 'Link copied.'],
    ['shared', null],
    ['dismissed', null],
    ['failed', 'Could not copy the link.'],
  ] as const)('says %s -> %s', (outcome, said) => {
    expect(shareNotice(outcome)).toBe(said)
  })
})
