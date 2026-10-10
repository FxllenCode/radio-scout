import { Screen } from '@/components/layout/Screen'
import { useGetReferenceQuery } from '@/store/api'

/**
 * The **Reference page** (#77, spec US 58) — the Operator's own codes, written
 * in markdown and rendered on the server.
 *
 * The HTML is inserted as it arrives because the server is what makes it safe
 * to: raw HTML in the markdown is escaped and every address is checked before
 * any of it is written (`src/identity/reference.rs`), so what reaches here is
 * markdown's own tags and nothing that runs. Rendering it there rather than
 * here also keeps a markdown library out of every phone's download.
 *
 * Reached from Settings, which offers it only when there is one; arriving here
 * without one — a bookmark from before the Operator took it down — says so.
 */
export function ReferenceScreen() {
  const { data, isError } = useGetReferenceQuery()

  return (
    <Screen title="Reference">
      {data ? (
        <article
          className="reference"
          // Safe by construction on the server — see the component note.
          dangerouslySetInnerHTML={{ __html: data.html }}
        />
      ) : (
        <p className="text-sm text-muted-foreground">
          {isError ? 'This scanner has no reference page.' : 'Reading…'}
        </p>
      )}
    </Screen>
  )
}
