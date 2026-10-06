/**
 * The `<iframe>` an Operator hands to another site (#75, spec US 59) — the
 * whole of what Settings → Admin → Embeds is for.
 *
 * Pure, because the snippet is pasted into somebody else's HTML and every rule
 * about it is worth stating as a value: which address it carries, that the
 * name is escaped for an attribute, and the attributes themselves.
 */
import type { AdminEmbed } from '@/types'

type Addressed = Pick<AdminEmbed, 'path' | 'url'>

/** Where the snippet points: the server's absolute address when `[server]
 *  public_url` gave it one, else this screen's own origin — which is the
 *  Operator's, and may be a LAN address no reader of the host's page can reach,
 *  so the screen says so beside it. */
export function embedAddress(row: Addressed, origin: string): string {
  return row.url ?? `${origin}${row.path}`
}

/** Escape for a double-quoted HTML attribute. */
function attribute(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('"', '&quot;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
}

/**
 * The snippet.
 *
 * - `title`, because a frame with none is one an assistive browser can only
 *   call "frame".
 * - A fixed height and a capped width — the page scrolls its list inside the
 *   frame, so a host's layout never moves as Calls arrive.
 * - `loading="lazy"`, so a frame below the fold of a busy homepage costs the
 *   Instance nothing until somebody scrolls to it.
 * - `allow="autoplay"`, so a browser that would otherwise ask again lets a
 *   reader who pressed Listen hear the *next* Call without another press.
 */
export function embedSnippet(row: Addressed & Pick<AdminEmbed, 'name'>, origin: string): string {
  return (
    `<iframe src="${attribute(embedAddress(row, origin))}" ` +
    `title="${attribute(row.name)}" width="100%" height="420" ` +
    'style="border:0;max-width:480px" loading="lazy" allow="autoplay">' +
    '</iframe>'
  )
}
