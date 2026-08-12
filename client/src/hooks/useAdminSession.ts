import { useGetAdminSessionQuery } from '@/store/api'

/**
 * Whether this browser holds a live admin session (#19, #49).
 *
 * Every admin screen needs this twice: `AdminGate` renders the password form
 * without it, and the screen's own queries must **not run** without it. The
 * second half is what this hook is for — a `useQuery` fires as soon as its
 * component mounts, and a component that renders the gate has still mounted, so
 * a signed-out visit to Talkgroups would otherwise fire four admin requests that
 * can only ever be 401s. Noise in the operator log (ADR-0011 rule 7 calls a 4xx
 * a WARN), and a screen that flashes "could not be read" before it asks for a
 * password.
 *
 * It costs nothing to call in several places: RTK Query dedupes subscribers to
 * one request and serves the rest from the cache.
 */
export function useAdminSession(): boolean {
  return useGetAdminSessionQuery().isSuccess
}
