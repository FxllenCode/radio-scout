import { Navigate, Route, Routes } from 'react-router-dom'

import { AppShell } from '@/components/layout/AppShell'
import { LiveScreen } from '@/routes/LiveScreen'
import { TalkgroupsScreen } from '@/routes/TalkgroupsScreen'
import { SearchScreen } from '@/routes/SearchScreen'
import { SettingsScreen } from '@/routes/SettingsScreen'

/** The app shell + the four bottom-tab destinations (docs/design/brief.md).
 *  Live (#11), Search (#13) and Talkgroups (#12) are built; Settings (#19) is
 *  still a placeholder its ticket fills in. */
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
