/** A stored Call as delivered over the live feed and the archive API. Mirrors
 *  the backend `StoredCall` (compact camelCase). Per CONTEXT.md, **Ref** is the
 *  recorder-supplied external id and **id** is Radio-Scout's internal key. */
export interface Call {
  id: number
  systemRef: number
  systemLabel?: string
  talkgroupRef: number
  talkgroupLabel?: string
  talkgroupGroup?: string
  talkgroupTag?: string
  /** The talkgroup's curated LED color, set by CSV import (#18). Absent until
   *  an operator curates it; `ledForCall` then falls back to the deterministic
   *  per-talkgroup color. */
  led?: string
  /** Talkgroup Refs this Call is patched to (rdio `patches[]`). */
  patches?: number[]
  frequency?: number
  source?: number
  dateTime?: string
  timestamp?: number
  audioMime?: string
  /** Where to fetch the audio (audio never rides the live-feed socket). */
  audioUrl: string
}

/** The archive-search filters, exactly as `GET /api/calls` takes them. */
export interface SearchQuery {
  /** Inclusive lower bound on call time, unix ms. */
  after?: number
  /** Inclusive upper bound on call time, unix ms. */
  before?: number
  system?: number
  talkgroup?: number
  group?: string
  tag?: string
  /** `oldest` is what playback mode walks: forwards through history. */
  sort?: 'newest' | 'oldest'
  limit?: number
  offset?: number
}

/** One page of `GET /api/calls`. Results are fully denormalized, so a page
 *  renders and plays without a follow-up request per Call. */
export interface SearchPage {
  results: Call[]
  /** Total matching Calls, ignoring the page window. */
  count: number
  limit: number
  offset: number
  hasMore: boolean
}

/** One Talkgroup a listener can select, as `GET /api/catalog` serves it (#12).
 *  Nested under its System, whose Ref completes the key the selection uses. */
export interface CatalogTalkgroup {
  ref: number
  label?: string
  name?: string
  /** The Talkgroup's single Tag (CONTEXT.md) — one half of the category rows. */
  tag?: string
  /** Every Group it belongs to, sorted — the other half. */
  groups: string[]
  /** The curated LED color (#18), when an operator has set one. */
  led?: string
}

export interface CatalogSystem {
  ref: number
  label?: string
  talkgroups: CatalogTalkgroup[]
}

/** `GET /api/catalog` — everything the Talkgroups panel offers. Unlike
 *  [`FilterOptions`], this is the *configured* world rather than the archived
 *  one: a Talkgroup whose Calls have aged out is still selectable. */
export interface Catalog {
  systems: CatalogSystem[]
}

export interface SystemOption {
  ref: number
  label?: string
}

export interface TalkgroupOption {
  systemRef: number
  ref: number
  label?: string
  tag?: string
}

/** `GET /api/calls/filters` — the values each filter can usefully take given
 *  the others already chosen. Only values with Calls behind them are offered. */
export interface FilterOptions {
  systems: SystemOption[]
  talkgroups: TalkgroupOption[]
  groups: string[]
  tags: string[]
  /** The span the current (non-date) filters can reach, unix ms. */
  dateStartMs?: number
  dateStopMs?: number
}
