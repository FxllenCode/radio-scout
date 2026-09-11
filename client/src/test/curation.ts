import { http, HttpResponse } from 'msw'

import type {
  AdminApiKey,
  AdminEvent,
  EventMember,
  FreezeReport,
  AdminShareLink,
  AdminDownstream,
  AdminWebhook,
  AdminLabel,
  AdminSystem,
  AdminTalkgroup,
  AdminUnit,
  MemberRef,
  MovedRef,
  Span,
  AdminToneProfile,
  ToneStep,
} from '@/types'
import { unusable } from '@/lib/tone'
import { ORIGIN } from './handlers'

/** An **in-memory instance** the curation screens can really be driven against
 *  (#49).
 *
 *  Handlers that answer a fixed document can only ever prove that a screen
 *  renders one; what these screens are for is *changing* things, and a change
 *  that is not visible afterwards is the bug worth catching. So this keeps
 *  state, applies the same rules the server applies — trimmed names, unique
 *  names, the blacklist living on the System and showing on the Talkgroup — and
 *  answers refusals in the server's own JSON shape.
 *
 *  It is deliberately **not** a reimplementation of the backend: `tests/curate.rs`
 *  proves the rules against the real thing on both dialects. This exists so a
 *  React test can assert that pressing a button sends the right request and
 *  shows what came back. */
export class FakeInstance {
  systems: AdminSystem[] = []
  talkgroups: AdminTalkgroup[] = []
  groups: AdminLabel[] = []
  tags: AdminLabel[] = []
  units: AdminUnit[] = []
  keys: AdminApiKey[] = []
  downstreams: AdminDownstream[] = []
  webhooks: AdminWebhook[] = []
  shares: AdminShareLink[] = []
  /** Events (#67), with their frozen members beside them for `members`' reason:
   *  the real listing deliberately keeps them off the rows. */
  events: AdminEvent[] = []
  eventCalls = new Map<number, EventMember[]>()
  /** An Event's share token, by id — kept here rather than on the row because
   *  the server never returns one from a listing, which is the property the
   *  screen is built on. */
  eventTokens = new Map<number, string>()
  /** Member Refs, by owning Talkgroup id (#50). Kept beside the rows rather
   *  than on them because the server deliberately keeps them off the listing —
   *  a query per row on a page of five hundred, for a column it cannot edit. */
  members = new Map<number, MemberRef[]>()
  /** A Unit's Ranges, by owning Unit id. */
  ranges = new Map<number, Span[]>()
  /** A channel's **Tone profiles** (#55), by owning Talkgroup id — beside the
   *  rows for `members`' reason: the server keeps them off the listing. */
  tones = new Map<number, AdminToneProfile[]>()
  /** Every request that changed something, in order — so a test can assert on
   *  *what was sent* as well as on what came back, which is the half that
   *  catches a form posting the wrong shape. */
  wrote: { method: string; path: string; body: unknown }[] = []
  private next = 1

  id(): number {
    return this.next++
  }

  system(row: Partial<AdminSystem> = {}): AdminSystem {
    const created: AdminSystem = {
      id: this.id(),
      ref: 11,
      label: 'Fulton',
      autoPopulate: false,
      blacklist: [],
      enhancement: null,
      talkgroups: 0,
      units: 0,
      calls: 0,
      createdAtMs: 1_700_000_000_000,
      ...row,
    }
    this.systems.push(created)
    return created
  }

  talkgroup(row: Partial<AdminTalkgroup> = {}): AdminTalkgroup {
    const system = this.systems[0] ?? this.system()
    const created: AdminTalkgroup = {
      id: this.id(),
      systemId: system.id,
      systemRef: system.ref,
      systemLabel: system.label,
      ref: 100,
      label: null,
      name: null,
      tag: null,
      groups: [],
      led: null,
      enhancement: null,
      blacklisted: false,
      calls: 0,
      createdAtMs: 1_700_000_000_000,
      ...row,
    }
    this.talkgroups.push(created)
    return created
  }

  label(kind: 'groups' | 'tags', row: Partial<AdminLabel> = {}): AdminLabel {
    const created: AdminLabel = {
      id: this.id(),
      name: 'Fire',
      talkgroups: 0,
      createdAtMs: 1_700_000_000_000,
      ...row,
    }
    this[kind].push(created)
    return created
  }

  unit(row: Partial<AdminUnit> = {}): AdminUnit {
    const system = this.systems[0] ?? this.system()
    const created: AdminUnit = {
      id: this.id(),
      systemId: system.id,
      systemRef: system.ref,
      systemLabel: system.label,
      ref: 1201,
      label: null,
      createdAtMs: 1_700_000_000_000,
      ...row,
    }
    this.units.push(created)
    return created
  }

  downstream(row: Partial<AdminDownstream> = {}): AdminDownstream {
    const created: AdminDownstream = {
      id: this.id(),
      label: 'county mirror',
      url: 'https://peer.example',
      scope: { all: false, sel: { 11: { '*': true } } },
      disabled: false,
      hasKey: true,
      queued: 0,
      lastSuccessMs: null,
      lastFailureMs: null,
      lastFailure: null,
      consecutiveFailures: 0,
      createdAtMs: 1_700_000_000_000,
      ...row,
    }
    this.downstreams.push(created)
    return created
  }

  /** A **Webhook** as the listing carries one — **no `url`**, which is the whole
   *  point of the shape: the fake answers with exactly the fields the real one
   *  does, so a screen that tried to read the credential back reads `undefined`
   *  here too. */
  webhook(row: Partial<AdminWebhook> = {}): AdminWebhook {
    const created: AdminWebhook = {
      id: this.id(),
      label: 'dispatch channel',
      host: 'discord.com',
      format: 'radio-scout',
      marks: ['emergency'],
      scope: { all: false, sel: { 11: { '*': true } } },
      disabled: false,
      queued: 0,
      lastSuccessMs: null,
      lastFailureMs: null,
      lastFailure: null,
      consecutiveFailures: 0,
      createdAtMs: 1_700_000_000_000,
      ...row,
    }
    this.webhooks.push(created)
    return created
  }

  /** One share link (#64). No token, for the real listing's reason: the link
   *  *is* the credential, so the server never returns it. */
  shareLink(row: Partial<AdminShareLink> = {}): AdminShareLink {
    const created: AdminShareLink = {
      id: this.id(),
      expiresAtMs: 1_700_600_000_000,
      createdAtMs: 1_700_000_000_000,
      expired: false,
      call: {
        id: 42,
        systemRef: 11,
        systemLabel: 'Fulton',
        talkgroupRef: 54241,
        talkgroupLabel: 'Fire Dispatch',
        talkgroupTag: 'Dispatch',
        timestamp: 1_699_999_000_000,
        audioUrl: '/api/call/42/audio',
      },
      ...row,
    }
    this.shares.push(created)
    return created
  }

  /** One Event, with however many frozen members it was given. */
  event(row: Partial<AdminEvent> = {}, members: Partial<EventMember>[] = []): AdminEvent {
    const id = this.id()
    const frozen: EventMember[] = members.map((member, index) => ({
      // The Call's id and the member's own are deliberately different numbers:
      // a fixture where they matched would model a document the server cannot
      // send, and would have hidden the collision that made them one key.
      id: 500 + index,
      memberId: this.id(),
      addedAtMs: 1_700_000_100_000,
      systemRef: 11,
      systemLabel: 'Fulton',
      talkgroupRef: 54241,
      talkgroupLabel: 'Fire Dispatch',
      timestamp: 1_699_999_000_000 + index * 1_000,
      audioUrl: `/api/admin/events/${id}/calls/${900 + index}/audio`,
      ...member,
    }))
    this.eventCalls.set(id, frozen)
    const created: AdminEvent = {
      id,
      name: 'Mill Street fire',
      calls: frozen.length,
      bytes: frozen.length * 1_000,
      shared: false,
      createdAtMs: 1_700_000_000_000,
      updatedAtMs: 1_700_000_000_000,
      ...row,
    }
    this.events.push(created)
    return created
  }

  key(row: Partial<AdminApiKey> = {}): AdminApiKey {
    const created: AdminApiKey = {
      id: this.id(),
      label: 'the pi',
      systemRef: null,
      disabled: false,
      createdAtMs: 1_700_000_000_000,
      ...row,
    }
    this.keys.push(created)
    return created
  }
}

/** The server's refusal shape — the document `curateFailure` reads back. */
export function refusal(
  status: number,
  error: string,
  detail: string,
  extra: Record<string, unknown> = {},
) {
  return HttpResponse.json({ error, detail, ...extra }, { status })
}

/** A signed-in admin session, plus every curation route, backed by `instance`.
 *
 *  Pass to `server.use(...)`. Returns handlers rather than installing them so a
 *  test can add its own refusal *after* these and have it win. */
export function curationHandlers(instance: FakeInstance) {
  const record = async (
    method: string,
    request: Request,
    path: string,
  ): Promise<Record<string, unknown>> => {
    const body = request.body ? await request.clone().json() : undefined
    instance.wrote.push({ method, path, body })
    return (body ?? {}) as Record<string, unknown>
  }

  return [
    http.get(`${ORIGIN}/api/admin/session`, () =>
      HttpResponse.json({ csrf_token: 'csrf-abc', expires_in_secs: 3600 }),
    ),
    http.post(
      `${ORIGIN}/api/admin/logout`,
      () => new HttpResponse(null, { status: 204 }),
    ),

    http.get(`${ORIGIN}/api/admin/systems`, () =>
      HttpResponse.json({ results: instance.systems }),
    ),
    http.post(`${ORIGIN}/api/admin/systems`, async ({ request }) => {
      const body = await record('POST', request, '/api/admin/systems')
      const ref =
        typeof body.ref === 'number'
          ? body.ref
          : Math.max(0, ...instance.systems.map((row) => row.ref)) + 1
      if (instance.systems.some((row) => row.ref === ref)) {
        return refusal(
          409,
          'system-ref-taken',
          `another system already answers to ${ref}`,
        )
      }
      return HttpResponse.json(
        instance.system({ ref, label: (body.label as string) ?? null }),
        { status: 201 },
      )
    }),
    http.patch(`${ORIGIN}/api/admin/systems/:id`, async ({ request, params }) => {
      const body = await record('PATCH', request, `/api/admin/systems/${params.id}`)
      const row = instance.systems.find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, 'system-not-found', 'no such system')
      Object.assign(row, body)
      return HttpResponse.json(row)
    }),
    http.delete(`${ORIGIN}/api/admin/systems/:id`, async ({ request, params }) => {
      const url = new URL(request.url)
      const path = `/api/admin/systems/${params.id}${url.search}`
      instance.wrote.push({ method: 'DELETE', path, body: undefined })
      const row = instance.systems.find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, 'system-not-found', 'no such system')
      if (row.calls > 0 && url.searchParams.get('force') !== 'true') {
        return refusal(
          409,
          'system-has-calls',
          `this system still has ${row.calls} calls in the archive`,
          { calls: row.calls },
        )
      }
      instance.systems = instance.systems.filter((it) => it !== row)
      return new HttpResponse(null, { status: 204 })
    }),

    http.get(`${ORIGIN}/api/admin/talkgroups`, ({ request }) => {
      const query = new URL(request.url).searchParams
      const system = query.get('system')
      const text = query.get('q')?.toLowerCase()
      const results = instance.talkgroups.filter(
        (row) =>
          (system === null || String(row.systemRef) === system) &&
          (text === undefined ||
            (row.label ?? '').toLowerCase().includes(text) ||
            String(row.ref) === text),
      )
      return HttpResponse.json({
        results,
        count: results.length,
        limit: 50,
        offset: 0,
        hasMore: false,
      })
    }),
    http.post(`${ORIGIN}/api/admin/talkgroups`, async ({ request }) => {
      const body = await record('POST', request, '/api/admin/talkgroups')
      if (instance.talkgroups.some((row) => row.ref === body.ref)) {
        return refusal(
          409,
          'talkgroup-ref-taken',
          `another talkgroup already answers to ${body.ref}`,
        )
      }
      return HttpResponse.json(
        instance.talkgroup({
          ref: body.ref as number,
          label: (body.label as string) ?? null,
        }),
        { status: 201 },
      )
    }),
    http.post(`${ORIGIN}/api/admin/talkgroups/assign`, async ({ request }) => {
      const body = await record('POST', request, '/api/admin/talkgroups/assign')
      const ids = (body.ids as number[]) ?? []
      for (const row of instance.talkgroups.filter((it) => ids.includes(it.id))) {
        for (const name of (body.addGroups as string[]) ?? []) {
          if (!row.groups.includes(name)) row.groups.push(name)
        }
        row.groups = row.groups.filter(
          (name) => !((body.removeGroups as string[]) ?? []).includes(name),
        )
        row.groups.sort()
        if ('tag' in body) row.tag = body.tag as string | null
      }
      return HttpResponse.json({ changed: ids.length })
    }),
    http.patch(`${ORIGIN}/api/admin/talkgroups/:id`, async ({ request, params }) => {
      const body = await record(
        'PATCH',
        request,
        `/api/admin/talkgroups/${params.id}`,
      )
      const row = instance.talkgroups.find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, 'talkgroup-not-found', 'no such talkgroup')
      if (body.led != null && !LED_PALETTE.includes(body.led as string)) {
        return refusal(400, 'unknown-led', `"${body.led}" is not an LED colour`)
      }
      Object.assign(row, body)
      // The blacklist lives on the System; the Talkgroup row only shows it.
      const system = instance.systems.find((it) => it.id === row.systemId)
      if (system && typeof body.blacklisted === 'boolean') {
        system.blacklist = body.blacklisted
          ? [...new Set([...system.blacklist, row.ref])].sort((a, b) => a - b)
          : system.blacklist.filter((ref) => ref !== row.ref)
      }
      return HttpResponse.json(row)
    }),
    http.delete(`${ORIGIN}/api/admin/talkgroups/:id`, async ({ request, params }) => {
      const url = new URL(request.url)
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/talkgroups/${params.id}${url.search}`,
        body: undefined,
      })
      const row = instance.talkgroups.find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, 'talkgroup-not-found', 'no such talkgroup')
      if (row.calls > 0 && url.searchParams.get('force') !== 'true') {
        return refusal(
          409,
          'talkgroup-has-calls',
          `this talkgroup still has ${row.calls} calls in the archive`,
          { calls: row.calls },
        )
      }
      instance.talkgroups = instance.talkgroups.filter((it) => it !== row)
      return new HttpResponse(null, { status: 204 })
    }),

    ...labelRoutes(instance, 'groups'),
    ...labelRoutes(instance, 'tags'),

    http.get(`${ORIGIN}/api/admin/units`, ({ request }) => {
      const query = new URL(request.url).searchParams
      const unnamed = query.get('unnamed')
      const results = instance.units.filter(
        (row) => unnamed !== 'true' || row.label == null,
      )
      return HttpResponse.json({
        results,
        count: results.length,
        limit: 50,
        offset: 0,
        hasMore: false,
      })
    }),
    http.post(`${ORIGIN}/api/admin/units`, async ({ request }) => {
      const body = await record('POST', request, '/api/admin/units')
      if (instance.units.some((row) => row.ref === body.ref)) {
        return refusal(
          409,
          'unit-ref-taken',
          `another unit already answers to ${body.ref}`,
        )
      }
      return HttpResponse.json(
        instance.unit({
          ref: body.ref as number,
          label: (body.label as string) ?? null,
        }),
        { status: 201 },
      )
    }),
    http.patch(`${ORIGIN}/api/admin/units/:id`, async ({ request, params }) => {
      const body = await record('PATCH', request, `/api/admin/units/${params.id}`)
      const row = instance.units.find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, 'unit-not-found', 'no such unit')
      Object.assign(row, body)
      return HttpResponse.json(row)
    }),
    http.delete(`${ORIGIN}/api/admin/units/:id`, ({ params }) => {
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/units/${params.id}`,
        body: undefined,
      })
      instance.units = instance.units.filter((it) => String(it.id) !== params.id)
      return new HttpResponse(null, { status: 204 })
    }),

    // -- Merge curation (#50) ---------------------------------------------

    http.get(`${ORIGIN}/api/admin/talkgroups/:id/members`, ({ params }) =>
      HttpResponse.json({ results: membersOf(instance, Number(params.id)) }),
    ),
    http.post(
      `${ORIGIN}/api/admin/talkgroups/:id/members`,
      async ({ request, params }) => {
        const url = new URL(request.url)
        const dryRun = url.searchParams.has('dryRun')
        const body = await record(
          'POST',
          request,
          `/api/admin/talkgroups/${params.id}/members${url.search}`,
        )
        const owner = instance.talkgroups.find((it) => String(it.id) === params.id)
        if (!owner) return refusal(404, 'talkgroup-not-found', 'no such talkgroup')
        return HttpResponse.json(
          applyDelta(
            instance,
            owner,
            (body.fold as number[]) ?? [],
            (body.unfold as number[]) ?? [],
            dryRun,
          ),
        )
      },
    ),

    http.get(`${ORIGIN}/api/admin/talkgroups/:id/tones`, ({ params }) =>
      HttpResponse.json({ results: instance.tones.get(Number(params.id)) ?? [] }),
    ),
    http.post(
      `${ORIGIN}/api/admin/talkgroups/:id/tones`,
      async ({ request, params }) => {
        const body = await record(
          'POST',
          request,
          `/api/admin/talkgroups/${params.id}/tones`,
        )
        const talkgroupId = Number(params.id)
        const steps = (body.steps as ToneStep[]) ?? []
        const tolerancePct = (body.tolerancePct as number) ?? 2
        const gapMaxMs = (body.gapMaxMs as number) ?? 300
        // The server's own rule, so a browser that let something through is a
        // test failure here rather than a surprise in production.
        const refused = unusable(steps, tolerancePct, gapMaxMs)
        if (refused) return refusal(400, 'unusable-tone-profile', refused)

        const created: AdminToneProfile = {
          id: instance.id(),
          talkgroupId,
          label: body.label as string,
          steps,
          tolerancePct,
          gapMaxMs,
          disabled: (body.disabled as boolean) ?? false,
          createdAtMs: 0,
        }
        instance.tones.set(talkgroupId, [
          ...(instance.tones.get(talkgroupId) ?? []),
          created,
        ])
        return HttpResponse.json(created, { status: 201 })
      },
    ),
    http.patch(`${ORIGIN}/api/admin/tones/:id`, async ({ request, params }) => {
      const body = await record('PATCH', request, `/api/admin/tones/${params.id}`)
      for (const [talkgroupId, held] of instance.tones) {
        const found = held.find((it) => String(it.id) === params.id)
        if (!found) continue
        const updated = { ...found, ...body } as AdminToneProfile
        instance.tones.set(
          talkgroupId,
          held.map((it) => (it.id === found.id ? updated : it)),
        )
        return HttpResponse.json(updated)
      }
      return refusal(404, 'tone-profile-not-found', 'no such tone profile')
    }),
    http.delete(`${ORIGIN}/api/admin/tones/:id`, async ({ request, params }) => {
      await record('DELETE', request, `/api/admin/tones/${params.id}`)
      for (const [talkgroupId, held] of instance.tones) {
        if (!held.some((it) => String(it.id) === params.id)) continue
        instance.tones.set(
          talkgroupId,
          held.filter((it) => String(it.id) !== params.id),
        )
        return new HttpResponse(null, { status: 204 })
      }
      return refusal(404, 'tone-profile-not-found', 'no such tone profile')
    }),

    http.get(`${ORIGIN}/api/admin/units/:id/ranges`, ({ params }) =>
      HttpResponse.json({ results: instance.ranges.get(Number(params.id)) ?? [] }),
    ),
    http.post(`${ORIGIN}/api/admin/units/:id/ranges`, async ({ request, params }) => {
      const body = await record(
        'POST',
        request,
        `/api/admin/units/${params.id}/ranges`,
      )
      const unit = instance.units.find((it) => String(it.id) === params.id)
      if (!unit) return refusal(404, 'unit-not-found', 'no such unit')
      const held = instance.ranges.get(unit.id) ?? []
      const add = (body.add as Span[]) ?? []
      const remove = (body.remove as Span[]) ?? []

      const clash = add.find((span) =>
        held.some(
          (owned) =>
            !remove.some((gone) => sameSpan(gone, owned)) &&
            span.from <= owned.to &&
            owned.from <= span.to,
        ),
      )
      if (clash) {
        const owned = held.find(
          (it) => clash.from <= it.to && it.from <= clash.to,
        )!
        return refusal(
          409,
          'range-overlaps',
          `${clash.from}-${clash.to} overlaps ${owned.from}-${owned.to}, ` +
            'which is already owned on this system',
        )
      }
      const kept = held.filter(
        (owned) => !remove.some((gone) => sameSpan(gone, owned)),
      )
      instance.ranges.set(unit.id, [...kept, ...add])
      return HttpResponse.json({
        added: add.length,
        removed: held.length - kept.length,
      })
    }),

    // -- The configuration document (#51) ----------------------------------

    http.get(`${ORIGIN}/api/admin/config`, () =>
      HttpResponse.json(
        {
          version: 1,
          systems: instance.systems.map((row) => ({
            ref: row.ref,
            label: row.label,
            autoPopulate: row.autoPopulate,
            talkgroups: instance.talkgroups
              .filter((it) => it.systemId === row.id)
              .map((it) => ({ ref: it.ref, label: it.label ?? undefined })),
          })),
        },
        {
          headers: {
            'content-disposition':
              'attachment; filename="radio-scout-config-2026-08-13.json"',
          },
        },
      ),
    ),
    http.post(`${ORIGIN}/api/admin/config/import`, async ({ request }) => {
      const url = new URL(request.url)
      const dryRun = url.searchParams.has('dryRun')
      const body = (await record(
        'POST',
        request,
        `/api/admin/config/import${url.search}`,
      )) as { systems?: DocumentSystem[] }

      let talkgroups = 0
      for (const entry of body.systems ?? []) {
        const system = dryRun
          ? { id: -1 }
          : (instance.systems.find((it) => it.ref === entry.ref) ??
            instance.system({ ref: entry.ref, label: entry.label ?? null }))
        for (const channel of entry.talkgroups ?? []) {
          talkgroups += 1
          if (!dryRun) {
            instance.talkgroup({
              systemId: system.id,
              ref: channel.ref,
              label: channel.label ?? null,
            })
          }
        }
      }
      return HttpResponse.json({
        dryRun,
        systems: { created: (body.systems ?? []).length, updated: 0, unchanged: 0 },
        talkgroups: { created: talkgroups, updated: 0, unchanged: 0 },
        units: { created: 0, updated: 0, unchanged: 0 },
        groupsCreated: 0,
        tagsCreated: 0,
        apiKeys: [],
        apiKeysToIssue: 0,
        downstreamsToKey: 0,
        rejected: [],
      })
    }),

    http.get(`${ORIGIN}/api/admin/api-keys`, () =>
      HttpResponse.json({ results: instance.keys }),
    ),
    http.post(`${ORIGIN}/api/admin/api-keys`, async ({ request }) => {
      const body = await record('POST', request, '/api/admin/api-keys')
      const row = instance.key({
        label: (body.label as string) ?? null,
        systemRef: (body.systemRef as number) ?? null,
      })
      // The one and only sight of it, exactly as the server answers a create.
      return HttpResponse.json({ ...row, key: 'issued-secret-0001' }, { status: 201 })
    }),
    http.patch(`${ORIGIN}/api/admin/api-keys/:id`, async ({ request, params }) => {
      const body = await record('PATCH', request, `/api/admin/api-keys/${params.id}`)
      const row = instance.keys.find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, 'api-key-not-found', 'no such API key')
      Object.assign(row, body)
      return HttpResponse.json(row)
    }),
    http.delete(`${ORIGIN}/api/admin/api-keys/:id`, ({ params }) => {
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/api-keys/${params.id}`,
        body: undefined,
      })
      instance.keys = instance.keys.filter((it) => String(it.id) !== params.id)
      return new HttpResponse(null, { status: 204 })
    }),

    // **Downstream** peers (#52). The listing carries each peer's health beside
    // it and — the thing worth modelling faithfully — never its key: the fake
    // answers with exactly the fields the real one does, so a screen that reads
    // a credential back reads `undefined` here too.
    http.get(`${ORIGIN}/api/admin/downstreams`, () =>
      HttpResponse.json({ results: instance.downstreams }),
    ),
    http.post(`${ORIGIN}/api/admin/downstreams`, async ({ request }) => {
      const body = await record('POST', request, '/api/admin/downstreams')
      const url = String(body.url ?? '').trim()
      if (url === '') return refusal(400, 'field-required', 'url is required')
      const apiKey = String(body.apiKey ?? '').trim()
      if (apiKey === '') {
        return refusal(400, 'field-required', 'apiKey is required')
      }
      const row = instance.downstream({
        label: (body.label as string) ?? null,
        url,
        scope: body.scope as AdminDownstream['scope'],
        disabled: (body.disabled as boolean) ?? false,
        hasKey: true,
      })
      return HttpResponse.json(row, { status: 201 })
    }),
    http.patch(`${ORIGIN}/api/admin/downstreams/:id`, async ({ request, params }) => {
      const body = await record('PATCH', request, `/api/admin/downstreams/${params.id}`)
      const row = instance.downstreams.find((it) => String(it.id) === params.id)
      if (!row) {
        return refusal(404, 'downstream-not-found', 'no such downstream')
      }
      // The key is write-only on the real surface: it goes in and never comes
      // back out, so the fake stores only the *fact* of one.
      const { apiKey, ...rest } = body as Record<string, unknown>
      if (typeof apiKey === 'string') row.hasKey = apiKey.trim() !== ''
      Object.assign(row, rest)
      if (row.disabled) row.queued = 0
      return HttpResponse.json(row)
    }),
    http.delete(`${ORIGIN}/api/admin/downstreams/:id`, ({ params }) => {
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/downstreams/${params.id}`,
        body: undefined,
      })
      instance.downstreams = instance.downstreams.filter(
        (it) => String(it.id) !== params.id,
      )
      return new HttpResponse(null, { status: 204 })
    }),

    // **Webhooks** (#54). The URL never comes back — see `FakeInstance.webhook`
    // — and the refusals modelled here are the two a form has to render beside
    // an input: an unusable address, and a mark this release does not know.
    http.get(`${ORIGIN}/api/admin/webhooks`, () =>
      HttpResponse.json({ results: instance.webhooks }),
    ),
    http.post(`${ORIGIN}/api/admin/webhooks`, async ({ request }) => {
      const body = await record('POST', request, '/api/admin/webhooks')
      const url = String(body.url ?? '').trim()
      if (url === '') return refusal(400, 'field-required', 'url is required')
      if (!url.startsWith('http://') && !url.startsWith('https://')) {
        return refusal(
          400,
          'unusable-webhook-url',
          'a webhook URL has to be absolute and start with https://',
        )
      }
      const marks = (body.marks as string[] | undefined) ?? []
      const unknown = marks.find((mark) => mark !== 'emergency')
      if (unknown != null) {
        return refusal(
          400,
          'unknown-mark',
          `"${unknown}" is not a mark a Call can carry: choose one of emergency`,
        )
      }
      const row = instance.webhook({
        label: (body.label as string) ?? null,
        host: url.split('://')[1]?.split(/[/?#]/)[0] ?? null,
        format: (body.format as AdminWebhook['format']) ?? 'radio-scout',
        marks: marks as AdminWebhook['marks'],
        scope: body.scope as AdminWebhook['scope'],
        disabled: (body.disabled as boolean) ?? false,
      })
      return HttpResponse.json(row, { status: 201 })
    }),
    http.patch(`${ORIGIN}/api/admin/webhooks/:id`, async ({ request, params }) => {
      const body = await record('PATCH', request, `/api/admin/webhooks/${params.id}`)
      const row = instance.webhooks.find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, 'webhook-not-found', 'no such webhook')
      // The URL is write-only on the real surface: it goes in and never comes
      // back, so the fake keeps only the host it implies.
      const { url, ...rest } = body as Record<string, unknown>
      if (typeof url === 'string') {
        row.host = url.split('://')[1]?.split(/[/?#]/)[0] ?? null
      }
      Object.assign(row, rest)
      if (row.disabled) row.queued = 0
      return HttpResponse.json(row)
    }),
    http.delete(`${ORIGIN}/api/admin/webhooks/:id`, ({ params }) => {
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/webhooks/${params.id}`,
        body: undefined,
      })
      instance.webhooks = instance.webhooks.filter(
        (it) => String(it.id) !== params.id,
      )
      return new HttpResponse(null, { status: 204 })
    }),

    // **Share links** (#64). A listing and a revoke, because that is the whole
    // surface: minting is the Listener's. The token never comes back — see
    // `FakeInstance.shareLink`.
    http.get(`${ORIGIN}/api/admin/shares`, ({ request }) => {
      const url = new URL(request.url)
      const limit = Number(url.searchParams.get('limit') ?? 50)
      const offset = Number(url.searchParams.get('offset') ?? 0)
      const results = instance.shares.slice(offset, offset + limit)
      return HttpResponse.json({
        results,
        count: instance.shares.length,
        limit,
        offset,
        hasMore: offset + results.length < instance.shares.length,
      })
    }),
    http.delete(`${ORIGIN}/api/admin/shares/:id`, ({ params }) => {
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/shares/${params.id}`,
        body: undefined,
      })
      const before = instance.shares.length
      instance.shares = instance.shares.filter(
        (it) => String(it.id) !== params.id,
      )
      if (instance.shares.length === before) {
        return refusal(404, 'share-link-not-found', 'no such share link')
      }
      return new HttpResponse(null, { status: 204 })
    }),

    // **Events** (#67). Unlike share links this is a full CRUD surface, because
    // curating an incident is the Operator's — the only write on the Instance
    // that spends storage **Retention** can never reclaim.
    http.get(`${ORIGIN}/api/admin/events`, ({ request }) => {
      const url = new URL(request.url)
      const limit = Number(url.searchParams.get('limit') ?? 50)
      const offset = Number(url.searchParams.get('offset') ?? 0)
      const results = instance.events.slice(offset, offset + limit)
      return HttpResponse.json({
        results,
        count: instance.events.length,
        limit,
        offset,
        hasMore: offset + results.length < instance.events.length,
      })
    }),
    http.get(`${ORIGIN}/api/admin/events/:id`, ({ params }) => {
      const event = eventOf(instance, params.id)
      if (!event) return refusal(404, 'event-not-found', 'no such event')
      return HttpResponse.json({
        ...event,
        members: instance.eventCalls.get(event.id) ?? [],
      })
    }),
    http.get(`${ORIGIN}/api/admin/events/:id/share`, ({ params }) => {
      const event = eventOf(instance, params.id)
      if (!event) return refusal(404, 'event-not-found', 'no such event')
      const token = instance.eventTokens.get(event.id)
      return HttpResponse.json({
        url: token ? `/e?t=${token}` : undefined,
        shared: token !== undefined,
      })
    }),
    http.post(`${ORIGIN}/api/admin/events`, async ({ request }) => {
      const body = (await request.json()) as {
        name: string
        notes?: string | null
        callIds?: number[]
      }
      instance.wrote.push({ method: 'POST', path: '/api/admin/events', body })
      if (body.name.trim() === '') {
        return refusal(400, 'field-required', 'name is required')
      }
      const created = instance.event({ name: body.name, notes: body.notes ?? undefined })
      return HttpResponse.json(
        { ...created, members: [], ...frozen(instance, created.id, body.callIds ?? []) },
        { status: 201 },
      )
    }),
    http.patch(`${ORIGIN}/api/admin/events/:id`, async ({ params, request }) => {
      const body = (await request.json()) as {
        name?: string
        notes?: string | null
        shared?: boolean
      }
      instance.wrote.push({
        method: 'PATCH',
        path: `/api/admin/events/${params.id}`,
        body,
      })
      const event = eventOf(instance, params.id)
      if (!event) return refusal(404, 'event-not-found', 'no such event')
      if (body.name !== undefined) {
        if (body.name.trim() === '') {
          return refusal(400, 'field-required', 'name is required')
        }
        event.name = body.name.trim()
      }
      if (body.notes !== undefined) event.notes = body.notes ?? undefined
      if (body.shared !== undefined) {
        // The server's own rule: off clears the token, so sharing again mints a
        // *different* link — this toggle is the only revoke there is.
        if (body.shared && !instance.eventTokens.has(event.id)) {
          instance.eventTokens.set(event.id, `tok-${instance.id()}`)
        }
        if (!body.shared) instance.eventTokens.delete(event.id)
        event.shared = body.shared
      }
      return HttpResponse.json(event)
    }),
    http.delete(`${ORIGIN}/api/admin/events/:id`, ({ params }) => {
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/events/${params.id}`,
        body: undefined,
      })
      const event = eventOf(instance, params.id)
      if (!event) return refusal(404, 'event-not-found', 'no such event')
      instance.events = instance.events.filter((it) => it.id !== event.id)
      instance.eventCalls.delete(event.id)
      instance.eventTokens.delete(event.id)
      return new HttpResponse(null, { status: 204 })
    }),
    http.post(`${ORIGIN}/api/admin/events/:id/calls`, async ({ params, request }) => {
      const body = (await request.json()) as { callIds: number[] }
      instance.wrote.push({
        method: 'POST',
        path: `/api/admin/events/${params.id}/calls`,
        body,
      })
      const event = eventOf(instance, params.id)
      if (!event) return refusal(404, 'event-not-found', 'no such event')
      const report = frozen(instance, event.id, body.callIds)
      return HttpResponse.json({
        ...event,
        members: instance.eventCalls.get(event.id) ?? [],
        ...report,
      })
    }),
    http.delete(
      `${ORIGIN}/api/admin/events/:id/calls/:member`,
      ({ params }) => {
        instance.wrote.push({
          method: 'DELETE',
          path: `/api/admin/events/${params.id}/calls/${params.member}`,
          body: undefined,
        })
        const event = eventOf(instance, params.id)
        if (!event) return refusal(404, 'event-not-found', 'no such event')
        const held = instance.eventCalls.get(event.id) ?? []
        const kept = held.filter((it) => String(it.memberId) !== params.member)
        if (kept.length === held.length) {
          return refusal(404, 'event-call-not-found', 'no such call in this event')
        }
        instance.eventCalls.set(event.id, kept)
        event.calls = kept.length
        return new HttpResponse(null, { status: 204 })
      },
    ),
  ]
}

function eventOf(instance: FakeInstance, id: unknown): AdminEvent | undefined {
  return instance.events.find((it) => String(it.id) === String(id))
}

/** Freeze these Calls into an Event, the way the server does: a Call already
 *  held is counted rather than duplicated, and one over 900 is treated as gone
 *  so a test can drive the report's other arms. */
function frozen(
  instance: FakeInstance,
  id: number,
  callIds: number[],
): { added: FreezeReport } {
  const held = instance.eventCalls.get(id) ?? []
  const report: FreezeReport = {
    frozen: 0,
    alreadyHeld: 0,
    missing: 0,
    unreadable: 0,
  }
  for (const callId of callIds) {
    if (held.some((member) => member.id === callId)) {
      report.alreadyHeld += 1
      continue
    }
    if (callId >= 900) {
      report.missing += 1
      continue
    }
    held.push({
      id: callId,
      memberId: instance.id(),
      addedAtMs: 1_700_000_100_000,
      systemRef: 11,
      systemLabel: 'Fulton',
      talkgroupRef: 54241,
      talkgroupLabel: 'Fire Dispatch',
      timestamp: 1_699_999_000_000,
      audioUrl: `/api/admin/events/${id}/calls/${callId}/audio`,
    })
    report.frozen += 1
  }
  instance.eventCalls.set(id, held)
  const event = instance.events.find((it) => it.id === id)
  if (event) {
    event.calls = held.length
    event.bytes = held.length * 1_000
  }
  return { added: report }
}

/** One System as a configuration document carries it. */
interface DocumentSystem {
  ref: number
  label?: string | null
  talkgroups?: { ref: number; label?: string | null }[]
}

/** The member Refs a channel answers to. */
function membersOf(instance: FakeInstance, id: number): MemberRef[] {
  return instance.members.get(id) ?? []
}

function sameSpan(left: Span, right: Span): boolean {
  return left.from === right.from && left.to === right.to
}

/** Fold and unfold, closely enough that a screen can be driven against it.
 *
 *  The one behaviour that matters here is the difference the preview exists to
 *  show: a Ref naming an existing channel is **folded** and carries that
 *  channel's Calls across, while a Ref naming nothing is merely **recorded**.
 *  The real rules — patch rows, chain folds, the archive — are
 *  `tests/merge.rs`'s over a real database on both dialects. */
function applyDelta(
  instance: FakeInstance,
  owner: AdminTalkgroup,
  fold: number[],
  unfold: number[],
  dryRun: boolean,
) {
  const held = membersOf(instance, owner.id)
  const moved: MovedRef[] = []
  let kept = held.filter((member) => !unfold.includes(member.ref))

  for (const member of held.filter((it) => unfold.includes(it.ref))) {
    moved.push({
      ref: member.ref,
      movement: 'unfolded',
      label: member.label ?? null,
      calls: 0,
      carried: [],
    })
    if (!dryRun) {
      instance.talkgroup({ ref: member.ref, label: member.label ?? null })
    }
  }
  for (const ref of fold.filter((it) => !kept.some((m) => m.ref === it))) {
    const source = instance.talkgroups.find(
      (row) => row.ref === ref && row.systemId === owner.systemId,
    )
    moved.push({
      ref,
      movement: source ? 'folded' : 'recorded',
      label: source?.label ?? null,
      calls: source?.calls ?? 0,
      carried: [],
    })
    kept = [...kept, { ref, label: source?.label ?? null }]
    if (!dryRun && source) {
      owner.calls += source.calls
      instance.talkgroups = instance.talkgroups.filter((it) => it !== source)
    }
  }
  if (!dryRun) instance.members.set(owner.id, kept)

  return {
    dryRun,
    folded: moved.filter((it) => it.movement === 'folded').length,
    unfolded: moved.filter((it) => it.movement === 'unfolded').length,
    callsRepointed: moved.reduce((total, it) => total + it.calls, 0),
    moved,
  }
}

/** The LED palette the server validates against (`curate::checked_led`). */
const LED_PALETTE = [
  'blue',
  'cyan',
  'green',
  'magenta',
  'orange',
  'red',
  'white',
  'yellow',
]

/** Groups and Tags behave identically, so their routes are written once — the
 *  same reason `src/curate/labels.rs` writes them once. */
function labelRoutes(instance: FakeInstance, kind: 'groups' | 'tags') {
  const noun = kind === 'groups' ? 'group' : 'tag'

  return [
    http.get(`${ORIGIN}/api/admin/${kind}`, () =>
      HttpResponse.json({ results: instance[kind] }),
    ),
    http.post(`${ORIGIN}/api/admin/${kind}`, async ({ request }) => {
      const body = (await request.clone().json()) as { name?: string }
      instance.wrote.push({
        method: 'POST',
        path: `/api/admin/${kind}`,
        body,
      })
      const name = (body.name ?? '').trim()
      if (name === '') {
        return refusal(400, 'field-required', 'name is required', {
          field: 'name',
        })
      }
      if (instance[kind].some((row) => row.name === name)) {
        return refusal(409, 'name-taken', `the name "${name}" is already taken`)
      }
      return HttpResponse.json(instance.label(kind, { name }), { status: 201 })
    }),
    http.patch(`${ORIGIN}/api/admin/${kind}/:id`, async ({ request, params }) => {
      const body = (await request.clone().json()) as { name?: string }
      instance.wrote.push({
        method: 'PATCH',
        path: `/api/admin/${kind}/${params.id}`,
        body,
      })
      const row = instance[kind].find((it) => String(it.id) === params.id)
      if (!row) return refusal(404, `${noun}-not-found`, `no such ${noun}`)
      const name = (body.name ?? '').trim()
      if (instance[kind].some((it) => it.name === name && it !== row)) {
        return refusal(409, 'name-taken', `the name "${name}" is already taken`)
      }
      row.name = name
      return HttpResponse.json(row)
    }),
    http.delete(`${ORIGIN}/api/admin/${kind}/:id`, ({ params }) => {
      instance.wrote.push({
        method: 'DELETE',
        path: `/api/admin/${kind}/${params.id}`,
        body: undefined,
      })
      instance[kind] = instance[kind].filter((it) => String(it.id) !== params.id)
      return new HttpResponse(null, { status: 204 })
    }),
  ]
}
