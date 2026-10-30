import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { LoaderCircle, PanelLeftClose, PanelLeftOpen } from 'lucide-react'
import { useServerActivity } from '@/api/queries/jobs'
import { useLibraries } from '@/api/queries/libraries'
import { useSystemInfo } from '@/api/queries/system'
import { useSession } from '@/lib/session'
import { ChatPanel } from '@/components/assistant/ChatPanel'
import { Logo } from './Logo'
import { footerNav, mainNav, type NavItem } from './nav'
import { UserMenu } from './UserMenu'

interface SidebarProps {
  collapsed: boolean
  onToggle: () => void
}

export function Sidebar({ collapsed, onToggle }: SidebarProps) {
  const busy = useBusyPaths()
  return (
    <aside
      className={clsx(
        'sticky top-0 flex h-dvh shrink-0 flex-col border-r border-line/60 bg-sidebar transition-[width] duration-200',
        collapsed ? 'w-16' : 'w-64',
      )}
    >
      <div className={clsx('flex h-16 items-center', collapsed ? 'justify-center' : 'px-5')}>
        <Link to="/" aria-label="Zur Startseite">
          <Logo compact={collapsed} />
        </Link>
      </div>

      <nav aria-label="Hauptnavigation" className="flex-1 overflow-y-auto px-2 py-2">
        <ul className="space-y-1">
          {mainNav.map((item) => (
            <NavLink key={item.to} item={item} collapsed={collapsed} busy={busy.has(item.to)} />
          ))}
        </ul>
      </nav>

      <div className="space-y-1 border-t border-line/60 px-2 py-3">
        <ChatPanel collapsed={collapsed} />
        <UserMenu collapsed={collapsed} />
        <ul className="space-y-1">
          {footerNav.map((item) => (
            <NavLink key={item.to} item={item} collapsed={collapsed} />
          ))}
        </ul>
        <button
          type="button"
          onClick={onToggle}
          className={clsx(
            'flex h-10 w-full items-center gap-3 rounded-lg text-sm text-muted transition-colors hover:bg-surface-2 hover:text-fg',
            collapsed ? 'justify-center' : 'px-3',
          )}
          aria-label={collapsed ? 'Seitenleiste ausklappen' : 'Seitenleiste einklappen'}
          title={collapsed ? 'Ausklappen' : 'Einklappen'}
        >
          {collapsed ? <PanelLeftOpen className="size-5" /> : <PanelLeftClose className="size-5" />}
          {!collapsed && <span>Einklappen</span>}
        </button>
        <ServerStatus collapsed={collapsed} />
      </div>
    </aside>
  )
}

/** Small connection indicator: green dot and version once the server answered. */
function ServerStatus({ collapsed }: { collapsed: boolean }) {
  const info = useSystemInfo()
  const text = info.isSuccess
    ? `${info.data.name} ${info.data.version}`
    : info.isError
      ? 'Server nicht erreichbar'
      : 'Verbinde…'
  const dot = info.isSuccess ? 'bg-emerald-400' : info.isError ? 'bg-red-500' : 'bg-subtle'
  return (
    <p
      data-testid="server-status"
      title={text}
      className={clsx('flex items-center gap-2 pt-1 text-[11px] text-subtle', collapsed ? 'justify-center' : 'px-3')}
    >
      <span className={clsx('size-1.5 shrink-0 rounded-full', dot)} aria-hidden="true" />
      {!collapsed && <span className="truncate">{text}</span>}
    </p>
  )
}

const LIBRARY_PATHS: Record<string, string> = {
  movies: '/movies',
  shows: '/shows',
  music: '/music',
  photos: '/photos',
  audiobooks: '/audiobooks',
}

/**
 * Sections whose library is being scanned (or still analysed while analysis runs),
 * so their entry can spin like a library in Plex' sidebar. Administrators only.
 */
function useBusyPaths(): Set<string> {
  const isAdmin = useSession()?.user.is_admin ?? false
  const work = useServerActivity(isAdmin).data ?? []
  const libraries = useLibraries().data ?? []
  const analysing = work.some((w) => w.kind === 'media.probe' && w.running > 0)
  const busy = new Set<string>()
  for (const library of libraries) {
    const scanning = work.some((w) => w.kind === 'library.scan' && w.library_id === library.id && w.running > 0)
    const path = LIBRARY_PATHS[library.kind]
    if (path && (scanning || (analysing && library.counts.unprobed > 0))) busy.add(path)
  }
  return busy
}

function NavLink({ item, collapsed, busy = false }: { item: NavItem; collapsed: boolean; busy?: boolean }) {
  const Icon = item.icon
  return (
    <li>
      <Link
        to={item.to}
        activeOptions={{ exact: item.to === '/' }}
        title={collapsed ? item.label : undefined}
        className={clsx(
          'group relative flex h-10 items-center gap-3 rounded-lg text-sm font-medium text-muted transition-colors hover:bg-surface-2 hover:text-fg',
          'data-[status=active]:bg-surface-3 data-[status=active]:text-fg',
          collapsed ? 'justify-center' : 'px-3',
        )}
      >
        <Icon className="size-5 shrink-0 group-data-[status=active]:text-accent" />
        <span className={clsx(collapsed && 'sr-only')}>{item.label}</span>
        {busy && (
          <LoaderCircle
            className={clsx('size-3.5 shrink-0 text-accent motion-safe:animate-spin', collapsed ? 'absolute top-1 right-1' : 'ml-auto')}
            aria-label="Wird gescannt"
            data-testid="library-busy"
          />
        )}
      </Link>
    </li>
  )
}
