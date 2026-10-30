import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { useSystemInfo } from '@/api/queries/system'
import { useSession } from '@/lib/session'
import { AccountSettings } from './AccountSettings'
import { AiSettings } from './AiSettings'
import { BackupSettings } from './BackupSettings'
import { DashboardSettings } from './DashboardSettings'
import { ImportSettings } from './ImportSettings'
import { InvitationsSettings } from './InvitationsSettings'
import { JobsSettings } from './JobsSettings'
import { LibrariesSettings } from './LibrariesSettings'
import { LiveTvSettings } from './LiveTvSettings'
import { LogsSettings } from './LogsSettings'
import { MetadataSettings } from './MetadataSettings'
import { NetworkSettings } from './NetworkSettings'
import { NotificationsSettings } from './NotificationsSettings'
import { SessionsSettings } from './SessionsSettings'
import { SsoSettings } from './SsoSettings'
import { SubtitleSettings } from './SubtitleSettings'
import { SETTINGS_GROUPS, type SettingsTab } from './tabs'
import { TraktSettings } from './TraktSettings'
import { TranscodingSettings } from './TranscodingSettings'
import { UsersSettings } from './UsersSettings'

const PAGES: Record<SettingsTab, () => React.ReactNode> = {
  account: AccountSettings,
  trakt: TraktSettings,
  dashboard: DashboardSettings,
  libraries: LibrariesSettings,
  metadata: MetadataSettings,
  subtitles: SubtitleSettings,
  transcoding: TranscodingSettings,
  ai: AiSettings,
  livetv: LiveTvSettings,
  network: NetworkSettings,
  users: UsersSettings,
  invitations: InvitationsSettings,
  sso: SsoSettings,
  notifications: NotificationsSettings,
  import: ImportSettings,
  backup: BackupSettings,
  jobs: JobsSettings,
  sessions: SessionsSettings,
  logs: LogsSettings,
}

/** Pages that use the full width (charts, live lists). */
const WIDE: SettingsTab[] = ['dashboard', 'sessions', 'logs', 'jobs']

/**
 * Settings in the Plex layout: a grouped list on the left, the page on the right
 * under "<Server> — <Page>". On small screens the list becomes a dropdown.
 */
export function SettingsPage() {
  const { tab = 'account' } = useSearch({ from: '/app/settings' })
  const navigate = useNavigate()
  const session = useSession()
  const server = useSystemInfo().data?.name ?? 'Flixnet'
  const isAdmin = session?.user.is_admin ?? false
  const groups = SETTINGS_GROUPS.map((g) => ({ ...g, tabs: g.tabs.filter((t) => !t.admin || isAdmin) })).filter(
    (g) => g.tabs.length > 0,
  )
  const visible = groups.flatMap((g) => g.tabs)
  const active = visible.find((t) => t.id === tab) ?? visible[0]!
  const Page = PAGES[active.id]

  return (
    <div className="flex min-h-full">
      <aside
        aria-label="Einstellungsbereiche"
        className="sticky top-0 hidden h-dvh w-60 shrink-0 overflow-y-auto border-r border-line/50 bg-sidebar/60 py-6 md:block"
      >
        <nav className="flex flex-col gap-5">
          {groups.map((group) => (
            <div key={group.title}>
              <h2 className="mb-1 px-6 text-[11px] font-bold tracking-[0.12em] text-subtle uppercase">{group.title}</h2>
              <ul>
                {group.tabs.map((t) => (
                  <li key={t.id}>
                    <Link
                      to="/settings"
                      search={{ tab: t.id }}
                      aria-current={t.id === active.id ? 'page' : undefined}
                      className={clsx(
                        'relative flex items-center px-6 py-1.5 text-sm transition-colors',
                        t.id === active.id
                          ? 'font-semibold text-fg before:absolute before:top-1/2 before:left-3 before:size-1.5 before:-translate-y-1/2 before:rounded-full before:bg-accent'
                          : 'text-muted hover:text-fg',
                      )}
                    >
                      {t.label}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </nav>
      </aside>

      <div className="min-w-0 flex-1 pb-16">
        <header className="mx-6 mt-8 mb-6 border-b border-line/50 pb-4 md:mx-10">
          <h1 className="text-2xl font-extrabold tracking-tight">
            <span className="text-muted">{server}</span>
            <span className="mx-2 text-subtle">—</span>
            {active.label}
          </h1>
          <label className="mt-4 block md:hidden">
            <span className="sr-only">Einstellungsbereich</span>
            <select
              value={active.id}
              onChange={(e) => void navigate({ to: '/settings', search: { tab: e.target.value as SettingsTab } })}
              className="h-10 w-full rounded-lg border border-line bg-surface-2 px-3 text-sm"
            >
              {groups.map((g) => (
                <optgroup key={g.title} label={g.title}>
                  {g.tabs.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.label}
                    </option>
                  ))}
                </optgroup>
              ))}
            </select>
          </label>
        </header>
        <div className={clsx('px-6 md:px-10', WIDE.includes(active.id) ? 'max-w-6xl' : 'max-w-4xl')}>
          <Page />
        </div>
      </div>
    </div>
  )
}
