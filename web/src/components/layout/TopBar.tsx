import { Link } from '@tanstack/react-router'
import { Settings } from 'lucide-react'
import { useSession } from '@/lib/session'
import { ActivityMenu } from './ActivityMenu'

const button =
  'relative flex size-10 items-center justify-center rounded-full bg-black/40 text-fg/90 backdrop-blur-md transition-colors hover:bg-black/60 hover:text-fg'

/**
 * Plex-style shortcuts in the top right of the media view: the activity bubble
 * (administrators) and the gear for the settings.
 */
export function TopBar() {
  const session = useSession()
  const isAdmin = session?.user.is_admin ?? false

  return (
    <div className="pointer-events-none fixed top-3 right-4 z-40 flex gap-2 md:right-6">
      {isAdmin && <ActivityMenu className={button} />}
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
