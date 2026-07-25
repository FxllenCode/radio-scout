/**
 * Next-Call prefetch (#14): download the audio the queue is about to play, so
 * the `<audio>` element starts it from cache instead of a round trip.
 *
 * Why a plain `fetch` and not a second `<audio preload="auto">`: **iOS Safari
 * ignores `preload`** and won't buffer media without a user gesture, so a hidden
 * element buys nothing on the platform that needs it most. A GET is not media,
 * so it runs — and the audio route serves `Cache-Control: private, max-age=…,
 * immutable`, which is what makes the element's later request a cache hit
 * (including the ranged one a media element actually sends).
 *
 * A Call is seconds of audio, so this is tens of kilobytes, and it only ever
 * runs for the single Call queued behind the current one.
 */

/** Warm the HTTP cache for `url`. Never rejects: a prefetch that fails just
 *  means the next Call loads the slow way. */
export async function prefetchAudio(
  url: string,
  signal?: AbortSignal,
): Promise<void> {
  try {
    const response = await fetch(url, { signal })
    // Read it to the end — an unread body may never reach the cache, which is
    // the entire point of the request.
    await response.arrayBuffer()
  } catch {
    // Aborted (the queue moved on) or offline. Either way, nothing to say.
  }
}
