import { Link } from '@tanstack/react-router'
import { Activity, Settings } from 'lucide-react'
import { useActiveSessions } from '@/api/queries/playstate'
import { useSession } from '@/lib/session'

const button =
  'relative flex size-10 items-center justify-center rounded-full bg-black/40 text-fg/90 backdrop-blur-md transition-colors hover:bg-black/60 hover:text-fg'

/**
 * Plex-style shortcuts in the top right of the media view: the activity icon with the
 * number of running streams opens the dashboard, the gear opens the settings.
 */
export function TopBar() {
  const session = useSession()
  const isAdmin = session?.user.is_admin ?? false
  const streams = useActiveSessions({ enabled: isAdmin, interval: 10_000 }).data?.length ?? 0

  return (
    <div className="pointer-events-none fixed top-3 right-4 z-40 flex gap-2 md:right-6">
      {isAdmin && (
        <Link
          to="/settings"
          search={{ tab: 'dashboard' }}
          className={`pointer-events-auto ${button}`}
          aria-label={streams > 0 ? `Dashboard, ${streams} ${streams === 1 ? 'Wiedergabe' : 'Wiedergaben'}` : 'Dashboard'}
          title="Dashboard"
          data-testid="activity-button"
        >
          <Activity className="size-5" aria-hidden="true" />
          {streams > 0 && (
            <span
              className="absolute -top-1 -right-1 flex h-5 min-w-5 items-center justify-center rounded-full bg-accent px-1 text-[11px] font-bold text-white tabular-nums"
              aria-hidden="true"
            >
              {streams}
            </span>
          )}
        </Link>
      )}
      <Link
        to="/settings"
        search={{ tab: isAdmin ? 'libraries' : 'account' }}
        className={`pointer-events-auto ${button}`}
        aria-label="Einstellungen"
        title="Einstellungen"
      >
        <Settings className="size-5" aria-hidden="true" />
      </Link>
    </div>
  )
}
