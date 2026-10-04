/**
 * The **Dirwatch** screen's pure half (#72): what each format is, which of the
 * form's fields it reads, and a watch's health as one line.
 *
 * Kept out of the component so the rules — which fields a format shows, what a
 * status means to an Operator — are tested without a DOM, and so the screen
 * never offers a field the server would ignore or refuse.
 */
import type { AdminDirwatch, DirwatchFormat, DirwatchStatus } from '@/types'

import { formatCallTime } from './archive'

/** One format, as the form offers it. */
export interface FormatChoice {
  value: DirwatchFormat
  label: string
  /** Which folder to point it at, in the Recorder's own words. */
  folder: string
  /** The audio it reads when the form names none — the server's default. */
  extension: string
}

/** Every format, in the order the form lists them — the Recorder the
 *  maintainer runs first. */
export const FORMATS: FormatChoice[] = [
  {
    value: 'trunk-recorder',
    label: 'Trunk Recorder',
    folder: 'its captureDir — keep audioArchive and callLog on',
    extension: 'wav',
  },
  {
    value: 'sdrtrunk',
    label: 'SDRTrunk',
    folder: 'its recordings folder, recording MP3',
    extension: 'mp3',
  },
  {
    value: 'dsdplus',
    label: 'DSDPlus Fast Lane',
    folder: 'its Record, 1R-Record or VC-Record folder',
    extension: 'mp3',
  },
  {
    value: 'mask',
    label: 'Filename mask',
    folder: 'any folder whose file names follow a pattern',
    extension: 'wav',
  },
]

/** The choice for `format` — the first for one this release does not list,
 *  which the server would have refused anyway. */
export function formatChoice(format: DirwatchFormat): FormatChoice {
  return FORMATS.find((choice) => choice.value === format) ?? FORMATS[0]
}

/** Which of the optional fields `format` reads at all. A field the format
 *  ignores is not drawn, so nothing on the form is decoration. */
export function fieldsFor(format: DirwatchFormat): {
  mask: boolean
  talkgroup: boolean
} {
  return {
    mask: format === 'mask',
    // Trunk Recorder's `.json` and SDRTrunk's tag always name their Talkgroup;
    // a mask or a DSDPlus name may not.
    talkgroup: format === 'mask' || format === 'dsdplus',
  }
}

/** The tokens a mask may use, for the form's hint. */
export const MASK_TOKENS =
  '#TG #TGAFS #TGHZ #TGKHZ #TGMHZ #TGLBL #SYS #SYSLBL #SITE #SITELBL #UNIT ' +
  '#UNITLBL #DATE #TIME #ZTIME #HZ #KHZ #MHZ #GROUP #TAG'

/** What a status means, in a sentence an Operator acts on. */
const STATUS: Record<DirwatchStatus, string> = {
  starting: 'starting',
  watching: 'watching',
  disabled: 'disabled',
  'outside-roots': 'not watching: outside [dirwatch] roots',
  'no-such-directory': 'not watching: the folder is gone',
  'cannot-watch': 'not watching: the system refused to watch it',
  unreadable: 'not watching: this release cannot read its settings',
}

/** A watch's health as one line. */
export function healthLine(row: AdminDirwatch): string {
  const parts = [formatChoice(row.format).label, STATUS[row.health.status]]
  if (row.deleteAfter) parts.push('deletes as it goes')
  parts.push(
    row.health.lastIngestMs == null
      ? `${row.health.ingested} ingested`
      : `${row.health.ingested} ingested, last ${formatCallTime(row.health.lastIngestMs)}`,
  )
  if (row.health.refused > 0) {
    parts.push(
      `${row.health.refused} refused${
        row.health.lastRefusal == null ? '' : ` · ${row.health.lastRefusal}`
      }`,
    )
  }
  return parts.join(' · ')
}

/** A whole number a form field holds, or `undefined` for a blank one — and
 *  `null` for one that is not a number, which the form refuses before asking. */
export function wholeNumber(text: string): number | undefined | null {
  const trimmed = text.trim()
  if (trimmed === '') return undefined
  return /^\d+$/.test(trimmed) ? Number(trimmed) : null
}
