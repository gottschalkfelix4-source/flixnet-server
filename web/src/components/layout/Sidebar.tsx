import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { PanelLeftClose, PanelLeftOpen } from 'lucide-react'
import { Logo } from './Logo'
import { footerNav, mainNav, type NavItem } from './nav'
import { UserMenu } from './UserMenu'

interface SidebarProps {
  collapsed: boolean
  onToggle: () => void
}

export function Sidebar({ collapsed, onToggle }: SidebarProps) {
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
            <NavLink key={item.to} item={item} collapsed={collapsed} />
          ))}
        </ul>
      </nav>

      <div className="space-y-1 border-t border-line/60 px-2 py-3">
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
      </div>
    </aside>
  )
}

function NavLink({ item, collapsed }: { item: NavItem; collapsed: boolean }) {
  const Icon = item.icon
  return (
    <li>
      <Link
        to={item.to}
        activeOptions={{ exact: item.to === '/' }}
        title={collapsed ? item.label : undefined}
        className={clsx(
          'group flex h-10 items-center gap-3 rounded-lg text-sm font-medium text-muted transition-colors hover:bg-surface-2 hover:text-fg',
          'data-[status=active]:bg-surface-3 data-[status=active]:text-fg',
          collapsed ? 'justify-center' : 'px-3',
        )}
      >
        <Icon className="size-5 shrink-0 group-data-[status=active]:text-accent" />
        <span className={clsx(collapsed && 'sr-only')}>{item.label}</span>
      </Link>
    </li>
  )
}
