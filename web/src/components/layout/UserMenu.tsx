import { useNavigate } from '@tanstack/react-router'
import clsx from 'clsx'
import { LogOut } from 'lucide-react'
import { useLogout } from '@/api/queries/auth'
import { useSession } from '@/lib/session'

/** Avatar with the user's initial plus a sign-out button. */
export function UserMenu({ collapsed }: { collapsed: boolean }) {
  const session = useSession()
  const logout = useLogout()
  const navigate = useNavigate()
  if (!session) return null

  const { name, is_admin } = session.user
  const signOut = () =>
    logout.mutate(undefined, { onSettled: () => navigate({ to: '/login', replace: true }) })

  return (
    <div className={clsx('flex items-center gap-3', collapsed ? 'flex-col py-1' : 'px-2 py-1')}>
      <span
        aria-hidden="true"
        className="flex size-9 shrink-0 items-center justify-center rounded-full bg-gradient-to-br from-accent to-orange-700 text-sm font-extrabold text-accent-fg"
      >
        {name.charAt(0).toUpperCase()}
      </span>
      {!collapsed && (
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-bold" data-testid="current-user">
            {name}
          </p>
          <p className="text-xs text-subtle">{is_admin ? 'Administrator' : 'Mitglied'}</p>
        </div>
      )}
      <button
        type="button"
        onClick={signOut}
        disabled={logout.isPending}
        aria-label="Abmelden"
        title="Abmelden"
        className="flex size-9 items-center justify-center rounded-lg text-muted transition-colors hover:bg-surface-2 hover:text-fg"
      >
        <LogOut className="size-4.5" />
      </button>
    </div>
  )
}
