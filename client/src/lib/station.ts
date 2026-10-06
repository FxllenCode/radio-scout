/**
 * The **Station stream** (#74, spec US 60): the scanner as a radio station.
 *
 * One URL plays a Selection forever as an MP3 — in VLC, a Sonos, a smart
 * speaker, a car — none of which can run this app. This is the URL, built from
 * what the Listener has chosen; what plays down it, and why it is MP3, is
 * `src/station/mod.rs`.
 *
 * It carries the **grant** this browser holds, because a speaker is a listener
 * the server has to scope like any other: without it, a channel this browser
 * unlocked would play on the phone and stay silent on the speaker. That makes
 * the URL as much a credential as the grant is — exactly as every audio URL
 * this browser plays already is (`hooks/useGrant`).
 */
import { GRANT_PARAM } from '@/lib/access'
import type { Selection } from '@/lib/selection'
import { encodeSelection } from '@/lib/selectionUrl'

/** Where a stream is played from — `crate::station::STATION_PATH`. */
export const STATION_PATH = '/api/station.mp3'

/** The query a stream URL carries: the Selection, in the spelling a share link
 *  uses, and the grant when this browser holds one. */
export function stationQuery(selection: Selection, grant?: string): string {
  const query = new URLSearchParams({ sel: encodeSelection(selection) })
  if (grant) query.set(GRANT_PARAM, grant)
  return query.toString()
}
