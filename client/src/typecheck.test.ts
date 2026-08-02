/**
 * What `npm run typecheck` actually looks at (#102).
 *
 * The typechecked set is the one thing a typechecker cannot tell you is wrong:
 * a project that resolves to no files reports success, at speed, forever.
 * `tsconfig.test.json` was in exactly that state — it set its own `include` and
 * inherited `tsconfig.app.json`'s `exclude`, which names the very patterns the
 * include had just found, so `tsc -p tsconfig.test.json --listFiles` reported
 * one file: `src/fonts.d.ts`. Every test file in the project had been unchecked
 * since the project was split out, and CI's `Client` job ran the same no-op.
 *
 * What that costs is not hypothetical. A bulk rename of a Redux action payload
 * left `received({ call: {…} })` behind after the payload had become a bare
 * `Call`; `tsc -b` passed, and the failure surfaced as a `TypeError` deep inside
 * `lib/artwork.ts` during a Browser Mode run. A mangled `received(call({ id: 9) })`
 * — a *syntax* error — passed too.
 *
 * So this asserts the set rather than the config: "a wrong payload in a test
 * file fails the typecheck" is not something a repository can commit an example
 * of, but "these files are checked" is, and it is the same promise. It also
 * catches regressions this ticket's own fix would not — a renamed glob, a newly
 * inherited exclude, or a project dropped from `tsconfig.json`'s `references`,
 * which is the door the first draft of this file left open.
 */
import { relative, resolve, sep } from 'node:path'
import { describe, expect, it } from 'vitest'
import * as ts from 'typescript'

/** The client root, which is where Vitest runs from. */
const ROOT = resolve('.')

const HOST: ts.ParseConfigFileHost = {
  ...ts.sys,
  onUnRecoverableConfigFileDiagnostic: (diagnostic) => {
    throw new Error(ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n'))
  },
}

/**
 * The files `tsc -p <config>` would load, as repository-relative paths.
 *
 * Resolved by TypeScript's own config machinery rather than by re-implementing
 * `include`/`exclude` here, so this agrees with the compiler by construction —
 * including how `extends` merges the two — and costs milliseconds, because
 * nothing is typechecked to find out.
 */
function typechecked(config: string): string[] {
  const parsed = ts.getParsedCommandLineOfConfigFile(resolve(config), {}, HOST)
  if (!parsed) throw new Error(`${config} did not resolve to a project`)
  return parsed.fileNames.map((file) => relative(ROOT, file).split(sep).join('/'))
}

/**
 * Everything `npm run typecheck` loads — **through the root's `references`**,
 * which is the list `tsc -b` walks and the only thing that makes a project part
 * of the gate.
 *
 * Asking each config directly would prove the weaker thing. `tsconfig.json` is a
 * solution file whose entire content is its references, so a project missing
 * from it is never entered however correct its own `include` is — the same
 * silence this ticket is about, through a different door. This was in fact the
 * first version of these tests, and dropping `./tsconfig.test.json` from the
 * references left every one of them green.
 */
function checkedByTheGate(): string[] {
  const root = ts.readConfigFile(resolve('tsconfig.json'), ts.sys.readFile)
  if (root.error) {
    throw new Error(ts.flattenDiagnosticMessageText(root.error.messageText, '\n'))
  }
  const references = (root.config as { references?: { path: string }[] }).references ?? []
  return references.flatMap((reference) => typechecked(reference.path))
}

describe('the typechecked projects (#102)', () => {
  /** One representative of each shape the test project claims to cover, so a
   *  glob that stops matching one of them fails here rather than going quiet. */
  it.each([
    ['a unit test', 'src/store/live.test.ts'],
    ['a component test', 'src/routes/LiveScreen.test.tsx'],
    ['a Browser Mode test', 'src/components/CallPlayer.browser.test.tsx'],
    ['a harness module', 'src/test/utils.tsx'],
  ])('checks %s', (_shape, file) => {
    expect(checkedByTheGate()).toContain(file)
  })

  /**
   * The same trap, one project over. `tsconfig.worker.json` names `src/sw.ts`
   * in its `include` and inherits an `exclude` that names it too — so the
   * service worker was typechecked by no project at all, while already being
   * excluded from coverage as a WebWorker scope jsdom cannot enter. The one file
   * in the client with neither.
   */
  it('checks the service worker', () => {
    expect(checkedByTheGate()).toContain('src/sw.ts')
  })

  /** The other half of the split, and the reason the two projects exist: tests
   *  may reach for `node:fs` and `node:zlib`, application code may not. A test
   *  project that swallowed the app's files would hand `Buffer` and `process` to
   *  everything the browser ships. */
  it('keeps test files out of the application project', () => {
    const app = typechecked('tsconfig.app.json')

    expect(app).toContain('src/store/live.ts')
    expect(app).not.toContain('src/store/live.test.ts')
    expect(app).not.toContain('src/test/utils.tsx')
  })
})
