/**
 * What a screen reader is told, and when (#78).
 *
 * The Live display is a scanner readout a sighted Listener *glances* at; a
 * Listener who cannot see it had nothing — the only live region on the screen
 * was the queue count, so a Call starting, the link dropping and the feed
 * coming back were all silent. These are the sentences for those moments,
 * decided here so the words are a function of the facts and testable without
 * a screen reader.
 */
import type { Call } from '@/types'

import { feedReadout, type FeedStatus, type ReadoutFacts } from './feed'
import { systemName, talkgroupName, unitName } from './call'

/** A Call starting, as one sentence: who, on what, keyed by whom — the order
 *  the display draws them in. An emergency is said first because it is drawn
 *  first; an archived Call says so, because otherwise it is indistinguishable
 *  from the feed. */
export function callAnnouncement(call: Call, fromArchive: boolean): string {
  const unit = unitName(call)
  const said = [talkgroupName(call), systemName(call), ...(unit ? [`unit ${unit}`] : [])].join(', ')
  const flagged = call.emergency ? `Emergency. ${said}` : said
  return fromArchive ? `From the archive: ${flagged}` : flagged
}

/**
 * The feed's standing, as one sentence.
 *
 * Keyed on the **cause** and deliberately blind to the link's progress: a
 * reconnect loop flips between connecting and gone every few seconds, and a
 * sentence that changed with it would be re-announced on every flip for as long
 * as the server is down. The two refinements that *are* causes — this device
 * offline, the server turning us away — borrow [`feedReadout`]'s headline, so
 * the words a Listener hears are the words a sighted one reads.
 */
export function feedAnnouncement(status: FeedStatus, facts: ReadoutFacts): string {
  switch (status) {
    case 'live':
      return 'Live feed connected.'
    case 'off':
      return 'Live feed switched off.'
    case 'playback':
      return 'Playing from the archive.'
    case 'down':
      return facts.offline || facts.refused
        ? feedReadout(status, 'offline', facts).empty.headline
        : 'Lost the link to the server. Retrying.'
  }
}
