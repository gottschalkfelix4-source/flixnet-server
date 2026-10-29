import { Outlet } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { readStored, writeStored } from '@/lib/storage'
import { Sidebar } from './Sidebar'

const COLLAPSED_KEY = 'flixnet.sidebar.collapsed'

/** Narrow screens always start with the icon rail. */
function initialCollapsed(): boolean {
  if (window.matchMedia('(max-width: 767px)').matches) return true
  return readStored(COLLAPSED_KEY, false)
}

export function AppShell() {
  const [collapsed, setCollapsed] = useState(initialCollapsed)

  useEffect(() => writeStored(COLLAPSED_KEY, collapsed), [collapsed])

  return (
    <div className="flex">
      <Sidebar collapsed={collapsed} onToggle={() => setCollapsed((c) => !c)} />
      <main className="min-w-0 flex-1">
        <Outlet />
      </main>
    </div>
  )
}
