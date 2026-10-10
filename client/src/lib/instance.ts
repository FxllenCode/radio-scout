/**
 * The **Instance identity** (#77) as the app reads it — what an Operator
 * curated about the Instance itself: its name, its help link, the Reference
 * page, and the defaults every screen starts from.
 *
 * The server's document arrives once per app open (`GET /api/instance`), and
 * until it does every screen is drawn with [`DEFAULT_INSTANCE`] — which is
 * exactly what an Instance nobody has named sends, so a slow network never
 * shows anything an unnamed Instance would not.
 */
import type { Instance } from '@/types'

/** What the product is called when the Operator has not named their Instance —
 *  the server's `identity::PRODUCT_NAME`. */
export const PRODUCT_NAME = 'Radio-Scout'

/** What an Instance nobody has curated sends, field for field. */
export const DEFAULT_INSTANCE: Instance = {
  timeFormat: '24h',
  panelSort: 'name',
  playbackGoesLive: false,
  dimmerSecs: 0,
  showListeners: false,
  reference: false,
}

/** What this Instance is called — the Operator's name, or the product's. */
export const instanceName = (instance: Pick<Instance, 'name'>): string =>
  instance.name ?? PRODUCT_NAME
