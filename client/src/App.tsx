import { Navigate, Route, Routes } from 'react-router-dom'

import { AppShell } from '@/components/layout/AppShell'
import { LiveScreen } from '@/routes/LiveScreen'
import { TalkgroupsScreen } from '@/routes/TalkgroupsScreen'
import { SearchScreen } from '@/routes/SearchScreen'
import { LogsScreen } from '@/routes/LogsScreen'
import { SettingsScreen } from '@/routes/SettingsScreen'
import { UnitScreen } from '@/routes/UnitScreen'

/** The app shell + the four bottom-tab destinations (docs/design/brief.md).
 *  Live (#11), Search (#13) and Talkgroups (#12) are built; Settings carries
 *  the server status, the notifications switch (#16) and the operator log
 *  (#30), with the rest still placeholders their tickets fill in. */
export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route index element={<LiveScreen />} />
        <Route path="talkgroups" element={<TalkgroupsScreen />} />
        <Route path="search" element={<SearchScreen />} />
        {/* One radio's history (#47, spec US 44), reached by tapping a unit
            label anywhere it renders. A route rather than a tab: it is always
            arrived at *from* a Call, never browsed to. */}
        <Route path="unit/:systemRef/:ref" element={<UnitScreen />} />
        <Route path="settings" element={<SettingsScreen />} />
        {/* Settings -> Logs (#30), behind the admin session the screen asks
            for itself. A child route rather than a tab: an operator opens it
            occasionally, and the four tabs are the listener's. */}
        <Route path="settings/logs" element={<LogsScreen />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}
