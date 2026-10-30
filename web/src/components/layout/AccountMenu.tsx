import { Link, useNavigate } from '@tanstack/react-router'
import { ChevronDown } from 'lucide-react'
import { useCallback, useId, useRef, useState } from 'react'
import { useLogout } from '@/api/queries/auth'
import { Avatar } from '@/components/ui/Avatar'
import { useSession } from '@/lib/session'
import { useDismiss } from './useDismiss'

const item = 'block w-full px-4 py-2 text-left text-sm text-muted transition-colors hover:bg-surface-3 hover:text-fg'

/** Avatar with a menu (like Plex): who is signed in, account, switch profile, sign out. */
export function AccountMenu() {
  const session = useSession()
  const logout = useLogout()
  const navigate = useNavigate()
  const [open, setOpen] = useState(false)
  const close = useCallback(() => setOpen(false), [])
  const root = useRef<HTMLDivElement>(null)
  useDismiss(root, open, close)
  const menuId = useId()
  if (!session) return null

  const { id, name, is_admin } = session.user
  const signOut = () => logout.mutate(undefined, { onSettled: () => navigate({ to: '/login', replace: true }) })

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-controls={menuId}
        aria-label={`Profilmenü (${name})`}
        title={name}
        className="flex h-10 items-center gap-1 rounded-full pr-1.5 pl-1 text-muted transition-colors hover:bg-surface-2 hover:text-fg"
      >
        <Avatar id={id} name={name} size="sm" className="size-8! text-xs!" />
        <span className="sr-only" data-testid="current-user">
          {name}
        </span>
        <ChevronDown className="size-4" aria-hidden="true" />
      </button>

      {open && (
        <div
          id={menuId}
          role="menu"
          aria-label="Profilmenü"
          className="absolute top-12 right-0 z-50 w-60 overflow-hidden rounded-lg border border-line bg-surface-2 pb-2 shadow-2xl"
        >
          <div className="flex flex-col items-center gap-2 border-b border-line/60 px-4 pt-5 pb-4 text-center">
            <Avatar id={id} name={name} />
            <div>
              <p className="font-bold">{name}</p>
              <p className="text-xs text-subtle">{is_admin ? 'Administrator' : 'Mitglied'}</p>
            </div>
          </div>
          <div className="pt-2">
            <Link to="/settings" search={{ tab: 'account' }} role="menuitem" className={item} onClick={close}>
              Kontoeinstellungen
            </Link>
            <Link to="/profiles" role="menuitem" className={item} onClick={close}>
              Profil wechseln
            </Link>
            <button type="button" role="menuitem" className={item} onClick={signOut} disabled={logout.isPending}>
              Abmelden
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
