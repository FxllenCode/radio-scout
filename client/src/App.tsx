import { Navigate, Route, Routes } from 'react-router-dom'

import { AppShell } from '@/components/layout/AppShell'
import { LiveScreen } from '@/routes/LiveScreen'
import { TalkgroupsScreen } from '@/routes/TalkgroupsScreen'
import { SearchScreen } from '@/routes/SearchScreen'
import { SettingsScreen } from '@/routes/SettingsScreen'

/** The app shell + the four bottom-tab destinations (docs/design/brief.md).
 *  Each screen is a placeholder that later tickets fill in:
 *  #11 Live · #12 Talkgroups · #13 Search · #17/#19 Settings/admin. */
export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route index element={<LiveScreen />} />
        <Route path="talkgroups" element={<TalkgroupsScreen />} />
        <Route path="search" element={<SearchScreen />} />
        <Route path="settings" element={<SettingsScreen />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}
