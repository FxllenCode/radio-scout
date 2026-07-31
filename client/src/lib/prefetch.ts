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
 * On the S3 blob backend the route answers a 307 to a presigned URL instead of
 * the bytes, and `fetch` follows it — so two things have to be cacheable for
 * this to pay off, and since #31 both are: the redirect carries a `max-age`
 * bounded by what the signature has left, and the *object* carries the same
 * `immutable` promise the proxied path sets. The element's later request then
 * hits the cached redirect and the cached bytes behind it. Nothing here has to
 * know which backend is in use.
 *
 * A Call is seconds of audio, so this is tens of kilobytes, and it only ever
 * runs for the single Call queued behind the current one.
 */

/** Warm the HTTP cache for `url`. Never rejects: a prefetch that fails just
 *  means the next Call loads the slow way.
 *
 *  `url` is optional because a Call may have no audio at all — an encrypted one
 *  (#42, spec US 9). Warming nothing is the whole of the right behavior there,
 *  so it is handled here rather than at each of the two call sites. */
export async function prefetchAudio(
  url: string | undefined,
  signal?: AbortSignal,
): Promise<void> {
  if (!url) return
  try {
    const response = await fetch(url, { signal })
    // Read it to the end — an unread body may never reach the cache, which is
    // the entire point of the request.
    await response.arrayBuffer()
  } catch {
    // Aborted (the queue moved on) or offline. Either way, nothing to say.
  }
}
