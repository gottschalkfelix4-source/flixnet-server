import { Outlet } from '@tanstack/react-router'
import clsx from 'clsx'
import { MiniPlayer } from '@/components/music/MiniPlayer'
import { usePlayerSelect } from '@/lib/music/usePlayer'
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

  // Room at the bottom so the music player bar never covers content.
  const hasQueue = usePlayerSelect((s) => s.queue.length > 0)

  return (
    <div className="flex">
      <Sidebar collapsed={collapsed} onToggle={() => setCollapsed((c) => !c)} />
      <main className={clsx('min-w-0 flex-1', hasQueue && 'pb-20')}>
        <Outlet />
      </main>
      <MiniPlayer />
    </div>
  )
}
