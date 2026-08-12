import { http, HttpResponse } from 'msw'

import type {
  AdminApiKey,
  AdminLabel,
  AdminSystem,
  AdminTalkgroup,
  AdminUnit,
} from '@/types'
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
  ]
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
