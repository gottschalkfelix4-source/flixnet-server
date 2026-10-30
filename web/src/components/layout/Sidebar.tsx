import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { LoaderCircle } from 'lucide-react'
import { useServerActivity } from '@/api/queries/jobs'
import { useLibraries } from '@/api/queries/libraries'
import { useSystemInfo } from '@/api/queries/system'
import { useSession } from '@/lib/session'
import { ChatPanel } from '@/components/assistant/ChatPanel'
import { mainNav, type NavItem } from './nav'

/** The media navigation below the top bar; collapses to an icon rail. */
export function Sidebar({ collapsed }: { collapsed: boolean }) {
  const busy = useBusyPaths()
  return (
    <aside
      className={clsx(
        'sticky top-14 flex h-[calc(100dvh-3.5rem)] shrink-0 flex-col border-r border-line/60 bg-sidebar transition-[width] duration-200',
        collapsed ? 'w-16' : 'w-64',
      )}
    >
      <nav aria-label="Hauptnavigation" className="flex-1 overflow-y-auto px-2 py-3">
        <ul className="space-y-0.5">
          {mainNav.map((item) => (
            <NavLink key={item.to} item={item} collapsed={collapsed} busy={busy.has(item.to)} />
          ))}
        </ul>
      </nav>

      <div className="space-y-1 border-t border-line/60 px-2 py-3">
        <ChatPanel collapsed={collapsed} />
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
          // Active entry like Plex: accent text and icon with a bar at the left edge.
          'before:absolute before:inset-y-2 before:-left-2 before:w-[3px] before:rounded-r-full data-[status=active]:text-accent data-[status=active]:before:bg-accent',
          collapsed ? 'justify-center' : 'px-3',
        )}
      >
        <Icon className="size-5 shrink-0" />
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
