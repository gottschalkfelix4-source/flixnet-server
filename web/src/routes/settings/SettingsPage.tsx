import { Link, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { PageHeader } from '@/components/ui/PageHeader'
import { useSession } from '@/lib/session'
import { AccountSettings } from './AccountSettings'
import { InvitationsSettings } from './InvitationsSettings'
import { JobsSettings } from './JobsSettings'
import { LibrariesSettings } from './LibrariesSettings'
import { LiveTvSettings } from './LiveTvSettings'
import { MetadataSettings } from './MetadataSettings'
import { SessionsSettings } from './SessionsSettings'
import { SubtitleSettings } from './SubtitleSettings'
import { SETTINGS_TABS as TABS } from './tabs'
import { TranscodingSettings } from './TranscodingSettings'
import { UsersSettings } from './UsersSettings'

export function SettingsPage() {
  const { tab = 'account' } = useSearch({ from: '/app/settings' })
  const session = useSession()
  const isAdmin = session?.user.is_admin ?? false
  const visible = TABS.filter((t) => !t.admin || isAdmin)
  const active = visible.some((t) => t.id === tab) ? tab : 'account'

  return (
    <div className="pb-16">
      <PageHeader title="Einstellungen" />
      <nav aria-label="Einstellungsbereiche" className="mb-6 flex gap-1 overflow-x-auto px-6 md:px-10">
        {visible.map((t) => (
          <Link
            key={t.id}
            to="/settings"
            search={{ tab: t.id }}
            className={clsx(
              'shrink-0 rounded-full px-4 py-2 text-sm font-semibold transition-colors',
              active === t.id ? 'bg-fg text-canvas' : 'text-muted hover:bg-surface-2 hover:text-fg',
            )}
          >
            {t.label}
          </Link>
        ))}
      </nav>
      <div className="max-w-4xl px-6 md:px-10">
        {active === 'account' && <AccountSettings />}
        {active === 'libraries' && <LibrariesSettings />}
        {active === 'metadata' && <MetadataSettings />}
        {active === 'subtitles' && <SubtitleSettings />}
        {active === 'transcoding' && <TranscodingSettings />}
        {active === 'livetv' && <LiveTvSettings />}
        {active === 'users' && <UsersSettings />}
        {active === 'invitations' && <InvitationsSettings />}
        {active === 'jobs' && <JobsSettings />}
        {active === 'sessions' && <SessionsSettings />}
      </div>
    </div>
  )
}
