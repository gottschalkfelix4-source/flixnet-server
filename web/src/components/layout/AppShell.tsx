import { Outlet, useRouterState } from '@tanstack/react-router'
import clsx from 'clsx'
import { MiniPlayer } from '@/components/music/MiniPlayer'
import { usePlayerSelect } from '@/lib/music/usePlayer'
import { useEffect, useState } from 'react'
import { rememberMediaPath } from '@/lib/lastMediaPath'
import { readStored, writeStored } from '@/lib/storage'
import { Sidebar } from './Sidebar'
import { TopBar } from './TopBar'

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

  // Settings are their own view (like Plex): the top bar stays, their own list
  // replaces the media sidebar.
  const location = useRouterState({ select: (s) => s.location })
  const inSettings = location.pathname.startsWith('/settings')
  // Where "Zurück zur Mediathek" in the settings leads: the last media page.
  useEffect(() => {
    if (!inSettings) rememberMediaPath(location.href)
  }, [inSettings, location.href])

  return (
    <>
      <TopBar collapsed={collapsed} onToggleSidebar={inSettings ? undefined : () => setCollapsed((c) => !c)} />
      <div className="flex pt-14">
        {!inSettings && <Sidebar collapsed={collapsed} />}
        <main className={clsx('min-w-0 flex-1', hasQueue && 'pb-20')}>
          <Outlet />
        </main>
        <MiniPlayer />
      </div>
    </>
  )
}
