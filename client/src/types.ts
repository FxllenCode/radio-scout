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
  frequency?: number
  source?: number
  dateTime?: string
  timestamp?: number
  audioMime?: string
  /** Where to fetch the audio (audio never rides the live-feed socket). */
  audioUrl: string
}
