/**
 * Nothing in the client is drawn under 11 pixels (#78).
 *
 * The critique pass that opened the ticket found 9- and 10-pixel type on the
 * Talkgroups panel's System headers, its sort and all-on controls, the search
 * presets, the DVR's rate switch and the sheets — read at arm's length, on a
 * phone, often at night. The ticket's criterion is "sub-11px interactive text
 * eliminated"; the floor here is the whole client, because "is this label part
 * of a control?" is exactly the judgement a later edit gets wrong, and a floor
 * nobody has to judge is one nothing slips under.
 *
 * Read off the source rather than a render: the admin screens and the sheets
 * are behind sessions and taps the browser pass (`phone.browser.test.tsx`)
 * does not walk, and a Tailwind arbitrary size is a literal in the source or it
 * is nowhere. Tailwind's named sizes start at `text-xs` (12px), so only the
 * bracketed ones can go under.
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative, resolve, sep } from 'node:path'
import { describe, expect, it } from 'vitest'

const SRC = resolve('src')

/** Every source file that can carry a class name. */
function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (name === 'node_modules') return []
    if (statSync(path).isDirectory()) return sources(path)
    return /\.(tsx?|css)$/.test(name) && !/\.test\.tsx?$/.test(name) ? [path] : []
  })
}

/** An arbitrary type size in pixels or rems, and how big it is in pixels. */
const SIZE = /\btext-\[(\d+(?:\.\d+)?)(px|rem)\]/g

describe('the type floor (#78)', () => {
  it('draws nothing under 11px, anywhere in the client', () => {
    const under: string[] = []
    for (const file of sources(SRC)) {
      const lines = readFileSync(file, 'utf8').split('\n')
      lines.forEach((line, index) => {
        for (const [, amount, unit] of line.matchAll(SIZE)) {
          const pixels = Number(amount) * (unit === 'rem' ? 16 : 1)
          if (pixels < 11) {
            const at = relative(resolve('.'), file).split(sep).join('/')
            under.push(`${at}:${index + 1} text-[${amount}${unit}]`)
          }
        }
      })
    }

    expect(under).toEqual([])
  })

  /** The scan must be looking at something, or it passes by reading nothing. */
  it('reads the screens it is guarding', () => {
    const read = sources(SRC).map((file) => relative(SRC, file).split(sep).join('/'))

    expect(read).toContain('routes/TalkgroupsScreen.tsx')
    expect(read).toContain('routes/admin/AdminTalkgroupsScreen.tsx')
    expect(read).toContain('index.css')
  })
})
