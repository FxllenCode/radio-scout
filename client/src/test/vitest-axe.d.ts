import 'vitest'

// Register the vitest-axe matcher added via `expect.extend` in setup.ts so
// `toHaveNoViolations()` type-checks under `tsc`.
interface AxeMatchers<R = unknown> {
  toHaveNoViolations(): R
}

declare module 'vitest' {
  interface Assertion<T = unknown> extends AxeMatchers<T> {}
  interface AsymmetricMatchersContaining extends AxeMatchers {}
}
