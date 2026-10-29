import { Link, useNavigate } from '@tanstack/react-router'
import clsx from 'clsx'
import { LogOut, Users } from 'lucide-react'
import { useLogout } from '@/api/queries/auth'
import { Avatar } from '@/components/ui/Avatar'
import { useSession } from '@/lib/session'

/** Current profile with shortcuts to switch profiles and to sign out. */
export function UserMenu({ collapsed }: { collapsed: boolean }) {
  const session = useSession()
  const logout = useLogout()
  const navigate = useNavigate()
  if (!session) return null

  const { id, name, is_admin } = session.user
  const signOut = () =>
    logout.mutate(undefined, { onSettled: () => navigate({ to: '/login', replace: true }) })
  const iconButton =
    'flex size-9 items-center justify-center rounded-lg text-muted transition-colors hover:bg-surface-2 hover:text-fg'

  return (
    <div className={clsx('flex items-center gap-2', collapsed ? 'flex-col py-1' : 'px-2 py-1')}>
      <Avatar id={id} name={name} size="sm" />
      {!collapsed && (
        <div className="min-w-0 flex-1 pl-1">
          <p className="truncate text-sm font-bold" data-testid="current-user">
            {name}
          </p>
          <p className="text-xs text-subtle">{is_admin ? 'Administrator' : 'Mitglied'}</p>
        </div>
      )}
      <Link to="/profiles" aria-label="Profil wechseln" title="Profil wechseln" className={iconButton}>
        <Users className="size-4.5" />
      </Link>
      <button
        type="button"
        onClick={signOut}
        disabled={logout.isPending}
        aria-label="Abmelden"
        title="Abmelden"
        className={iconButton}
      >
        <LogOut className="size-4.5" />
      </button>
    </div>
  )
}
