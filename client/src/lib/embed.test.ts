import { describe, expect, it } from 'vitest'

import { embedAddress, embedSnippet } from './embed'

const ROW = { name: 'Fire dispatch', path: '/embed?t=a1b2', url: null }

describe('an embed address', () => {
  /** With `[server] public_url` set, the server's absolute address is the one a
   *  stranger's page must carry. */
  it('is the server-made one when there is one', () => {
    expect(
      embedAddress(
        { ...ROW, url: 'https://scanner.example.org/embed?t=a1b2' },
        'http://192.168.1.5:3000',
      ),
    ).toBe('https://scanner.example.org/embed?t=a1b2')
  })

  /** ...and without one, the origin the Operator is on — which the screen
   *  then says is a guess. */
  it('is built from where the screen is when there is not', () => {
    expect(embedAddress(ROW, 'http://192.168.1.5:3000')).toBe(
      'http://192.168.1.5:3000/embed?t=a1b2',
    )
  })
})

describe('an embed snippet', () => {
  it('frames the address, named for a screen reader, and lazily', () => {
    const snippet = embedSnippet(ROW, 'https://scanner.example.org')

    expect(snippet).toBe(
      '<iframe src="https://scanner.example.org/embed?t=a1b2" ' +
        'title="Fire dispatch" width="100%" height="420" ' +
        'style="border:0;max-width:480px" loading="lazy" allow="autoplay">' +
        '</iframe>',
    )
  })

  /** The name goes into an attribute of somebody else's HTML, so it is escaped
   *  for one: a quote in it must not end the attribute. */
  it('escapes a name that would break out of its attribute', () => {
    const snippet = embedSnippet(
      { ...ROW, name: `Bob's "Fire" & <Rescue>` },
      'https://scanner.example.org',
    )

    expect(snippet).toContain(
      'title="Bob\'s &quot;Fire&quot; &amp; &lt;Rescue&gt;"',
    )
  })
})
