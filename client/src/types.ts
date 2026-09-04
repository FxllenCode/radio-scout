import type { QuietSpan } from '@/lib/catchup'

/** A stored Call as delivered over the live feed and the archive API. Mirrors
 *  the backend `StoredCall` (compact camelCase). Per CONTEXT.md, **Ref** is the
 *  recorder-supplied external id and **id** is Radio-Scout's internal key.
 *
 *  **What the client reads, not everything the wire carries** (#89). The server
 *  also sends `audioMime`, which nothing here needs — the `<audio>` element
 *  sniffs the response's own Content-Type. Declaring a field nobody reads
 *  invites code to be written against it when a better source is already in use.
 *
 *  A `dateTime` was declared here too, and removed for that reason. #98 then
 *  removed it from the *server*, where it turned out never to have been sent at
 *  all: the field was on both sides of the protocol and hardcoded absent on the
 *  sending one. `timestamp` is the only call time there is. */
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
  /** The Ref of the **first radio heard** on this Call (#47, spec US 42) — the
   *  canonical one, so a portable inside a fleet's Range reads as the apparatus
   *  that owns it.
   *
   *  This replaced a `source`, which was the rdio dialect's singular field and
   *  was therefore absent on every Trunk Recorder Call there has ever been (TR
   *  sends a `srcList` instead). Absent here means nobody was heard at all. */
  unitRef?: number
  /** What to call that radio: the **Unit**'s curated alias, else the name this
   *  Call arrived with. Absent when nobody has named it — which is every radio
   *  in an uncurated archive, and why the Ref is a separate field. */
  unitLabel?: string
  timestamp?: number
  /** How long the transmission is, in milliseconds (#42, spec US 8) — from the
   *  recorder's metadata or an audio-header parse at ingest. Absent when
   *  neither could say, which is every Call stored before #42. */
  durationMs?: number
  /** The radio set the emergency bit on this transmission. Absent when it
   *  didn't, which is nearly always. */
  emergency?: boolean
  /** Somebody starred this Call (#66, spec US 37). Absent when nobody has,
   *  which is nearly every Call there is.
   *
   *  **The Instance's mark, not this browser's**: a Star takes no credential to
   *  leave, so recording *who* left it would mean either a per-browser record
   *  of what somebody kept or a table anybody could fill by POSTing in a loop
   *  (`src/star.rs`). What this session has done to it lives in `store/stars`,
   *  and is read through `selectStarred` rather than off here. */
  starred?: boolean
  /** The talkgroup was encrypted, so this Call is metadata and nothing else
   *  (spec US 9) — there is no `audioUrl` on one. */
  encrypted?: boolean
  /** A **Tone profile** on this channel was paged in this Call's audio (#55,
   *  spec US 20). Absent when it wasn't, which is nearly always.
   *
   *  Kept beside `tones` rather than derived from it: this is what the row
   *  says, where the list is what the pages say. */
  tone?: boolean
  /** **Which** stations were paged, and where in the Call (#55). Absent unless
   *  one was, which is nearly always — a page-out happens a few times a day, so
   *  this key is on essentially no Call.
   *
   *  The station's name rather than only the flag, because "a page happened" is
   *  not the question: an operator with twelve stations on one dispatch channel
   *  is asking *who*. The label is the profile's as it was when the page fired,
   *  so a profile renamed later does not rewrite history. */
  tones?: TonePage[]
  /** Where nobody is talking, so **Catch-up** can skip it (#59, spec US 23) —
   *  `[[startMs, endMs], …]`, in order and disjoint.
   *
   *  Absent from most Calls: the server only reports a gap long enough to be
   *  worth a seek, and somebody saying one thing has none. **Also absent from
   *  every live frame**, because the frame is published at ingest and the scan
   *  happens behind it — `GET /api/calls/quiet` is how a queued Call gets these,
   *  and `store/live`'s `quietFound` is what writes them on. A Call read back
   *  from the Archive carries them already. */
  quiet?: QuietSpan[]
  /** The Site (tower) this was heard on, for multi-site systems (spec US 11). */
  siteRef?: number
  /** What that tower is called (#48, spec US 13). Independent of `siteRef`:
   *  a recorder sends a Ref and no name, and mining SDRTrunk's ID3 finds a
   *  name with no Ref beside it. */
  siteLabel?: string
  /** Where to fetch the audio (audio never rides the live-feed socket).
   *
   *  **Absent when there is nothing to fetch** — an encrypted Call. That is
   *  what keeps an unplayable Call out of the listening queue by construction:
   *  the queue is built from Calls that have audio, not from Calls that don't
   *  carry a flag. */
  audioUrl?: string
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
  /** Hide anything shorter than this many **whole seconds** (#42, spec US 8) —
   *  the kerchunk filter. A Call whose duration was never measured never
   *  matches; leaving this unset still shows every Call there is. */
  minDuration?: number
  /** Only Calls a given radio was heard on (#47, spec US 44). When a **Unit**
   *  owns that Ref, the search reaches every other Ref the apparatus answers
   *  to — its Ranges and its lone member Refs. */
  unit?: number
  /** Only Calls carrying this **mark** (#42, #55) — an emergency, or a
   *  tone-out. Single-valued because every other filter here combines with AND
   *  and a list would have to mean OR. */
  mark?: Mark
  /** Only the Calls somebody starred (#66, spec US 37). Absent and `false` are
   *  one answer — there is no "show me the unstarred ones", which would be the
   *  whole archive minus a handful and read as no filter at all. */
  starred?: boolean
  /** Only Calls a **Selection** reaches (#63) — the **DVR**'s scope, spelled
   *  the way a share link spells one (`lib/selectionUrl`). Answered with the
   *  live feed's own rule, so a channel reached through a **Patch** counts;
   *  `talkgroup` above compares the Call's canonical channel and stops. */
  sel?: string
  /** `oldest` is what playback mode walks: forwards through history. */
  sort?: 'newest' | 'oldest'
  limit?: number
  offset?: number
}

/**
 * One number per bucket across a stretch of time (#62, spec US 34–35, 41).
 *
 * Two charts are drawn from this shape and it says nothing about which: the
 * density ribbon over search results and the hour-by-day heatmap read Calls per
 * bucket, and the operator's chart reads peak Listeners per bucket. The axis
 * rides with the values because the client has to place them — and it is the
 * axis the server *resolved*, which may be coarser than the one asked for.
 */
export interface Series {
  /** The first instant covered, inclusive. */
  fromMs: number
  /** The first instant past the end, exclusive. */
  toMs: number
  bucketMs: number
  /** One value per bucket, oldest first. Never empty. */
  values: number[]
}

/**
 * What a chart asks for: a search's own filters, plus how finely to cut them.
 *
 * The filters are exactly `SearchQuery`'s, because the ribbon has to describe
 * the results above it rather than something that resembles them. `limit` and
 * `offset` are deliberately never sent — a window into a page says nothing
 * about how many Calls there were, and leaving them off is also what keeps the
 * ribbon out of the refetch a page turn causes.
 */
export interface ActivityQuery extends Omit<SearchQuery, 'limit' | 'offset'> {
  /** Buckets exactly this wide. What the heatmap needs: folding into local
   *  hours is only exact when a bucket is an hour. */
  bucketMs?: number
  /** About this many buckets across the range. What the ribbon needs, because
   *  what it really has is a width in pixels. */
  buckets?: number
}

/** What the operator's listener chart asks for (#62, spec US 41): a range and a
 *  grain, and no filters at all — there is nothing to filter a count by, which
 *  is the point. */
export interface ListenerQuery {
  after?: number
  before?: number
  bucketMs?: number
  buckets?: number
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
  /** Calls this Talkgroup took in the catalog's [`Catalog.activityWindowMs`]
   *  (#57). Absent for one it heard nothing on, which is why the panel reads it
   *  as `0` rather than the server sending a column of zeroes to every phone. */
  recentCalls?: number
  /** The newest of those Calls — what a last-heard age is a subtraction from.
   *  Never older than the window: it is the window's answer, not the
   *  Archive's. */
  lastCallAtMs?: number
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
  /** How far back [`CatalogTalkgroup.recentCalls`] counts (#57). On the wire
   *  rather than spelled again here, so "12 calls in the last 24 hours" stays
   *  true if the server's window ever moves. */
  activityWindowMs: number
  /** Whether this Instance mints **Share links** (#64, spec US 32). The share
   *  control is drawn from this: a control that is offered and then refused is
   *  a control that lies. */
  sharing: boolean
  /** Whether a range of the archive can be taken away, and how much of one at a
   *  time (#65, spec US 33). Here for `sharing`'s reason, and one further: the
   *  *number* is what lets the control say "that range holds 4,312 calls"
   *  before the wait rather than after it. */
  export: { enabled: boolean; maxCalls: number }
  /** What a Star is worth here (#66, spec US 37) — whether one holds a Call
   *  back from Retention, and for how many days past the transmission (`0`
   *  being for good). Here for `sharing`'s reason, one step on: a control that
   *  quietly means less than a Listener thinks it does is the same lie told
   *  more slowly, and only the server knows `[retention] starred_days`. */
  starred: { kept: boolean; keptDays: number }
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

/** A span of Refs an entity answers to (CONTEXT.md: **Range**), both ends
 *  inclusive. A lone member Ref is a span of one. */
export interface RefSpan {
  from: number
  to: number
}

/** One Talkgroup a radio has been heard on, and how much. */
export interface UnitTalkgroup {
  ref: number
  label?: string
  calls: number
  lastHeardMs: number
}

/** One radio's history, as `GET /api/unit/{systemRef}/{ref}` serves it (#47,
 *  spec US 44).
 *
 *  A *summary*, not a page of Calls: the Calls themselves are an ordinary
 *  archive search with `unit` set, which already pages and plays. */
export interface UnitHistory {
  systemRef: number
  systemLabel?: string
  ref: number
  label?: string
  /** The Ranges and lone member Refs this apparatus also answers to (#45).
   *  Absent when it owns none, which is every Unit until somebody merges one. */
  memberRefs?: RefSpan[]
  callCount: number
  /** Absent together when the radio has never keyed — which a curated Unit
   *  genuinely can be. */
  firstHeardMs?: number
  lastHeardMs?: number
  /** Busiest first: which channel a radio lives on is the question. */
  talkgroups: UnitTalkgroup[]
}

/** One stored log event (#30, ADR-0011), as `GET /api/admin/logs` serves it.
 *
 *  Its *parts*, never a rendered sentence: `fields` is the structured half rule
 *  6 insists on, and `requestId` is #28's correlation id — the value in an
 *  `internal error (request id: …)` a listener read out, so an operator with no
 *  shell can still find the cause. */
export interface LogEvent {
  id: number
  /** When it was recorded, unix milliseconds. */
  atMs: number
  /** `ERROR`, `WARN` or `INFO` — the sink stores nothing quieter (rule 5). */
  level: string
  /** The module it came from, e.g. `radio_scout::ingest`. */
  target: string
  message: string
  fields?: Record<string, unknown>
  requestId?: string
}

/** One page of `GET /api/admin/logs`, newest first. */
export interface LogPage {
  results: LogEvent[]
  /** Total events matching the filters, ignoring the page window. */
  count: number
  limit: number
  offset: number
  hasMore: boolean
}

/** What the Logs view filters on. Blank means "no filter", which is what the
 *  form's own empty state produces. */
export interface LogQuery {
  /** A severity **floor**: `warn` means warnings and errors. */
  level?: string
  after?: number
  before?: number
  limit?: number
  offset?: number
}

/** `GET /api/admin/session` / `POST /api/admin/login` (#19) — a live admin
 *  session. The CSRF token is held in memory, never in a cookie: it is a
 *  synchronizer token, which is what a sibling subdomain cannot forge. */
export interface AdminSession {
  csrf_token: string
  expires_in_secs: number
}

// ---------------------------------------------------------------------------
// Curation (#49, spec US 45–46) — the entities an Operator runs from a browser.
// ---------------------------------------------------------------------------

/** A small collection answered whole. The curation counterpart to a `Page`, and
 *  deliberately the same `results` key: Systems, Groups, Tags and API keys are
 *  counted in tens on the largest instance anybody runs, so paging them would be
 *  a control that never did anything. */
export interface Listing<T> {
  results: T[]
}

/** One page of a collection that really does need one. Structurally a `LogPage`
 *  over a different row — the shape both server read surfaces already answer
 *  with. */
export interface CuratedPage<T> {
  results: T[]
  count: number
  limit: number
  offset: number
  hasMore: boolean
}

/** One **System**, as the curation screen lists it. */
export interface AdminSystem {
  id: number
  ref: number
  label?: string | null
  autoPopulate: boolean
  /** Talkgroup Refs never ingested here — canonical, sorted, de-duplicated. */
  blacklist: number[]
  /** `null` inherits the instance's `[enhancement] mode` (#20); a plain boolean
   *  has no way to say "follow the instance". */
  enhancement?: boolean | null
  talkgroups: number
  units: number
  /** Calls in the Archive under it — what a delete would take. */
  calls: number
  createdAtMs: number
}

/** One **Talkgroup**. `blacklisted` is derived from the System's list rather
 *  than being a column of its own. */
export interface AdminTalkgroup {
  id: number
  systemId: number
  systemRef: number
  systemLabel?: string | null
  ref: number
  label?: string | null
  name?: string | null
  tag?: string | null
  groups: string[]
  led?: string | null
  enhancement?: boolean | null
  blacklisted: boolean
  calls: number
  createdAtMs: number
}

/** One **Group** or **Tag** — the two name-only entities. */
export interface AdminLabel {
  id: number
  name: string
  /** How many Talkgroups are behind it: what an Operator about to delete one
   *  needs to know. */
  talkgroups: number
  createdAtMs: number
}

/** One **Unit** — a radio's roster entry. */
export interface AdminUnit {
  id: number
  systemId: number
  systemRef: number
  systemLabel?: string | null
  ref: number
  label?: string | null
  createdAtMs: number
}

/** One **API key**. Never the secret: it is stored hashed and the listing has
 *  no field for it. */
export interface AdminApiKey {
  id: number
  label?: string | null
  /** The System Ref it may ingest into; `null` grants every System. */
  systemRef?: number | null
  disabled: boolean
  createdAtMs: number
}

/** A key that has just been issued — the row plus **the one and only sight of
 *  the secret**. */
export interface IssuedApiKey extends AdminApiKey {
  key: string
}

/** The **Selection** matrix (ADR-0004): `all` plus exceptions under
 *  `sel[system][talkgroup | "*"]`.
 *
 *  It lives here rather than in `lib/liveFeed` because three things are scoped
 *  by it and one of them is an admin type: a Listener's live feed, their Web
 *  Push subscription, and — since #52 — a **Downstream** peer. One declaration,
 *  so the algebra in `lib/selection.ts` applies to all three by construction.
 *  `Subscription` and `Selection` remain the names each surface knows it by. */
export interface SelectionMatrix {
  all: boolean
  sel: Record<string, Record<string, boolean>>
}

/** One **Downstream** peer (#52) — another instance this one forwards matching
 *  Calls to.
 *
 *  **Never the key it issued us.** Unlike our own roster the server has to store
 *  that one recoverably, because it goes on the wire on every delivery; what it
 *  does not do is hand it back, so there is no field for it here and no future
 *  edit to this type can start rendering one. `hasKey` is the diagnostic that
 *  replaces it: "refusing everything" and "never given a credential" are
 *  different problems and look identical without it. */
export interface AdminDownstream {
  id: number
  label?: string | null
  url: string
  /** Which Calls reach it — the live feed's own **Selection**. */
  scope: SelectionMatrix
  disabled: boolean
  hasKey: boolean
  /** **The durable queue depth**: Calls written down and not yet taken. Survives
   *  a restart, unlike anything the sender holds in memory. */
  queued: number
  lastSuccessMs?: number | null
  lastFailureMs?: number | null
  /** The last failure as a slug plus its status — `sink-refused (401)`. */
  lastFailure?: string | null
  consecutiveFailures: number
  createdAtMs: number
}

/** What a new peer needs. `apiKey` is write-only: it goes up and never comes
 *  back, and a `PATCH` that omits it leaves the stored one alone — which is what
 *  makes re-scoping a peer possible without re-typing a credential the screen
 *  can never show again. */
export interface NewDownstream {
  label?: string | null
  url: string
  apiKey: string
  scope: SelectionMatrix
  disabled?: boolean
}

/** A **mark** a Call can carry, as the wire spells it (#54, #55).
 *
 *  The whole client is written against the list rather than against the
 *  members — `MARKS` below is the only place that has to grow — which is what
 *  made #55's `tone` a one-line change everywhere but `markName`, where a
 *  `Record<Mark, string>` turns it into a compile error until somebody gives it
 *  a word. */
export type Mark = 'emergency' | 'tone'

/** Every mark this release knows, in the order a form offers them. */
export const MARKS: readonly Mark[] = ['emergency', 'tone']

/** Which shape a **Webhook**'s body takes (#54). */
export type WebhookFormat = 'radio-scout' | 'discord'

/** Every format, in the order a form offers them. */
export const WEBHOOK_FORMATS: readonly WebhookFormat[] = [
  'radio-scout',
  'discord',
]

/** A configured **Webhook** as the admin listing carries one (#54).
 *
 *  **There is no `url`**, and the omission is the point: a webhook's URL *is*
 *  its credential (a Discord one ends in a token), so it goes up once and never
 *  comes back — the `AdminDownstream.apiKey` rule, one notch stricter, because
 *  here there is no separate public half. `host` is what replaces it: enough to
 *  tell two rows apart, and not a secret. */
export interface AdminWebhook {
  id: number
  label?: string | null
  /** The URL's host — `discord.com`. Absent when the stored value is not a URL
   *  at all, which is a webhook that will never deliver. */
  host?: string | null
  format: WebhookFormat
  /** Which marks it asked for. Empty fires for nothing, which the screen shows
   *  rather than hides. */
  marks: Mark[]
  /** Which Calls can reach it — the live feed's own **Selection**. */
  scope: SelectionMatrix
  disabled: boolean
  /** **The durable queue depth**: Calls written down and not yet posted. */
  queued: number
  lastSuccessMs?: number | null
  lastFailureMs?: number | null
  /** The last failure as a slug plus its status — `sink-refused (404)`. */
  lastFailure?: string | null
  consecutiveFailures: number
  createdAtMs: number
}

/**
 * A minted **Share link** (#64, spec US 32), as the Listener who asked for it is
 * handed it back.
 *
 * `url` is a **path with its query** — `/s?t=…` — not an absolute URL: the
 * browser is the side that knows which origin it reached this Instance on, and
 * an Instance may not know its own address at all. `lib/share`'s `linkTo` makes
 * it absolute against `window.location.origin`, which is what a Listener pastes.
 */
export interface ShareLink {
  url: string
  expiresAtMs: number
}

/**
 * One share link as the Operator's screen lists it.
 *
 * **There is no token**, and the omission is the point — the [`AdminWebhook`]
 * rule, one row along: the link *is* the credential, and a screen that showed it
 * would be a place it could leak from that has nothing to do with sharing a
 * Call. What is shown instead is the Call it opens, which is the thing an
 * Operator is actually asking about.
 */
export interface AdminShareLink {
  id: number
  expiresAtMs: number
  /** When this Call first started being shared — untouched by a re-share, so the
   *  column answers "since when" rather than "who clicked most recently". */
  createdAtMs: number
  /** Whether it has already run out, decided against the **Instance's** clock
   *  rather than the browser's. */
  expired: boolean
  call: Call
}

/** What a new webhook needs. `url` is write-only, for [`AdminWebhook`]'s
 *  reason — and a `PATCH` that omits it leaves the stored one alone, which is
 *  the only thing that makes re-scoping possible at all. */
export interface NewWebhook {
  label?: string | null
  url: string
  format: WebhookFormat
  marks: Mark[]
  scope: SelectionMatrix
  disabled?: boolean
}

/** What the Talkgroups listing filters on. Blank is no filter, which is what the
 *  form's own empty state produces. */
export interface AdminTalkgroupQuery {
  /** A System **Ref** — what an Operator reads on the screen. */
  system?: number
  q?: string
  group?: string
  tag?: string
  blacklisted?: boolean
  limit?: number
  offset?: number
}

/** What the Units listing filters on. */
export interface AdminUnitQuery {
  system?: number
  q?: string
  /** Only radios nobody has named — the working set for naming a fleet, and
   *  unreachable from a text search. */
  unnamed?: boolean
  limit?: number
  offset?: number
}

/** One **member Ref** a channel answers to besides its own (#50, spec US 17).
 *
 *  `label` is what the folded channel called itself, so the list reads as the
 *  names an Operator curated — and `null` for a Ref that was never a channel of
 *  its own, which is the honest difference between "TAC 3, folded in" and
 *  "8123, written down". */
export interface MemberRef {
  ref: number
  label?: string | null
}

/** One **Range** of radio ids an apparatus answers to, both ends inclusive. */
export interface Span {
  from: number
  to: number
}

/** A merge edit: fold these Refs in, unfold those, and say nothing about
 *  anything else.
 *
 *  A delta rather than the finished set on purpose. A form is submitted by a tab
 *  that read the list some minutes ago, and inferring an unmerge from absence is
 *  exactly the rdio-scanner failure the curation surface exists to not repeat. */
export interface MemberDelta {
  fold?: number[]
  unfold?: number[]
}

/** The same, for a Unit's Ranges. */
export interface RangeDelta {
  add?: Span[]
  remove?: Span[]
}

/** Which way one Ref went. `recorded` is the arm the counts cannot express: a
 *  Ref nothing has been heard on yet was written down, absorbing nothing — and
 *  it is what a Ref belonging to another **System** looks like, since a Ref is
 *  unique only within one. */
export type Movement = 'folded' | 'recorded' | 'unfolded'

/** One Ref's share of a merge, as the confirmation renders it. */
export interface MovedRef {
  ref: number
  movement: Movement
  label?: string | null
  /** Calls this Ref would move. The number the whole preview exists for. */
  calls: number
  /** Member Refs the absorbed channel owned, which come across with it. */
  carried: number[]
}

/** What a fold did — or, when `dryRun`, would do. */
export interface MergeReport {
  dryRun: boolean
  folded: number
  unfolded: number
  callsRepointed: number
  moved: MovedRef[]
}

/** What a Range edit did. No preview: a Call names the radios it heard by Ref,
 *  never by a Unit's id, so nothing here moves one. */
export interface RangeReport {
  added: number
  removed: number
}

/** How one kind of row fared in a document import (#51, spec US 47). */
export interface Applied {
  created: number
  updated: number
  /** Rows the Instance already had exactly — the count that proves a re-import
   *  is a no-op, and so that a half-finished restore is safe to retry. */
  unchanged: number
}

/** One entry the import would not take, and **where it is** — a path into the
 *  document, which is a JSON file's answer to a CSV's line number. */
export interface RejectedEntry {
  at: string
  reason: string
  detail: string
}

/** What a document import did — or, when `dryRun`, would do. */
export interface DocumentReport {
  dryRun: boolean
  systems: Applied
  talkgroups: Applied
  units: Applied
  groupsCreated: number
  tagsCreated: number
  /** Keys this import issued, each shown once. Empty on a preview: showing a
   *  secret and then rolling it back is worse than showing none. */
  apiKeys: IssuedApiKey[]
  /** How many keys the roster is short — counted on a preview too, which is the
   *  only thing it can honestly say about a credential it must not create. */
  apiKeysToIssue: number
  /** **Downstream** peers this import added, each disabled until it is given the
   *  key its own peer issued — a backup carries a peer's shape and never its
   *  credential (#52). */
  downstreamsToKey: number
  rejected: RejectedEntry[]
}

/** One bulk action over selected Talkgroup rows (spec US 46). */
export interface AdminAssignment {
  ids: number[]
  addGroups?: string[]
  removeGroups?: string[]
  /** `null` clears the Tag; omitted leaves it alone. */
  tag?: string | null
}

/** One tone in a **Tone profile**'s sequence (#55) — a frequency, held for at
 *  least this long. */
export interface ToneStep {
  hz: number
  minMs: number
}

/** One **Tone profile** match on a Call (#55): which station was paged, and how
 *  far into the Call.
 *
 *  The label is the profile's **as it was when the page fired** — a profile
 *  renamed next year does not rewrite last year's pages, and one deleted does
 *  not erase them. */
export interface TonePage {
  label: string
  atMs: number
}

/** A station's paging sequence, as `GET /api/admin/talkgroups/{id}/tones`
 *  lists one (#55, spec US 20).
 *
 *  An **ordered sequence** rather than a fixed A/B pair, which is what lets one
 *  shape spell a single long group tone, Quick Call II's two, and an
 *  A-B-then-group run of three. */
export interface AdminToneProfile {
  id: number
  talkgroupId: number
  label: string
  steps: ToneStep[]
  /** How far off a tone may be, as a percentage of the tone. */
  tolerancePct: number
  /** How long a silence between two tones may be before the sequence is
   *  broken. */
  gapMaxMs: number
  disabled: boolean
  createdAtMs: number
}

/** A new Tone profile, as the form posts one. */
export interface NewToneProfile {
  label: string
  steps: ToneStep[]
  tolerancePct?: number
  gapMaxMs?: number
  disabled?: boolean
}
