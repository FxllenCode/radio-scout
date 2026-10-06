/** The **Embed** page's entry (#75): read the token off the frame's own
 *  address and mount. Everything else is `mount.ts`, which is tested. */
import './embed.css'

import { mountEmbed } from './mount'

mountEmbed(document.getElementById('embed')!, {
  token: new URLSearchParams(location.search).get('t'),
})
