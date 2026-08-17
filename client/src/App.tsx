import { Navigate, Route, Routes } from 'react-router-dom'

import { AppShell } from '@/components/layout/AppShell'
import { LiveScreen } from '@/routes/LiveScreen'
import { TalkgroupsScreen } from '@/routes/TalkgroupsScreen'
import { SearchScreen } from '@/routes/SearchScreen'
import { LogsScreen } from '@/routes/LogsScreen'
import { SettingsScreen } from '@/routes/SettingsScreen'
import { UnitScreen } from '@/routes/UnitScreen'
import { AdminScreen } from '@/routes/admin/AdminScreen'
import { AdminTalkgroupsScreen } from '@/routes/admin/AdminTalkgroupsScreen'
import { ApiKeysScreen } from '@/routes/admin/ApiKeysScreen'
import { DownstreamsScreen } from '@/routes/admin/DownstreamsScreen'
import { GroupsScreen, TagsScreen } from '@/routes/admin/LabelsScreen'
import { SystemsScreen } from '@/routes/admin/SystemsScreen'
import { UnitsScreen } from '@/routes/admin/UnitsScreen'

/** The app shell + the four bottom-tab destinations (docs/design/brief.md).
 *  Live (#11), Search (#13) and Talkgroups (#12) are built; Settings carries
 *  the server status, the operator log (#30)
 *  and — since #49 — the admin section, which is where an Instance is run from
 *  a browser instead of over SSH. */
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
        {/* Settings -> Admin (#49, spec US 45-46). Every one of these sits
            behind the session `AdminGate` renders, mirroring the server
            mounting its guard as a prefix layer: a screen added beside them
            inherits the gate rather than remembering it. */}
        <Route path="settings/admin" element={<AdminScreen />} />
        <Route path="settings/admin/systems" element={<SystemsScreen />} />
        <Route path="settings/admin/talkgroups" element={<AdminTalkgroupsScreen />} />
        <Route path="settings/admin/groups" element={<GroupsScreen />} />
        <Route path="settings/admin/tags" element={<TagsScreen />} />
        <Route path="settings/admin/units" element={<UnitsScreen />} />
        <Route path="settings/admin/api-keys" element={<ApiKeysScreen />} />
        <Route
          path="settings/admin/downstreams"
          element={<DownstreamsScreen />}
        />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}
