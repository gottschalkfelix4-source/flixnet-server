import { Link } from '@tanstack/react-router'
import { EllipsisVertical } from 'lucide-react'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { type ItemCard, useRefreshItem, useSetPlayed } from '@/api/queries/items'
import { useSession } from '@/lib/session'
import { AddToPlaylistDialog } from './AddToPlaylistDialog'

const PLAYABLE = ['movie', 'episode']
const item = 'block w-full px-4 py-2 text-left text-sm text-muted transition-colors hover:bg-surface-3 hover:text-fg'

/**
 * The "⋮" of a poster card (like Plex): play, mark watched, add to a playlist,
 * refresh metadata. The menu is placed in a portal so scrolling rows don't clip it.
 */
export function CardMenu({ card, onOpenChange }: { card: ItemCard; onOpenChange?: (open: boolean) => void }) {
  const [open, setOpenState] = useState(false)
  const [playlist, setPlaylist] = useState(false)
  const [position, setPosition] = useState<{ top: number; right: number } | null>(null)
  const button = useRef<HTMLButtonElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const setPlayed = useSetPlayed()
  const refresh = useRefreshItem()
  const isAdmin = useSession()?.user.is_admin ?? false

  const setOpen = (value: boolean) => {
    setOpenState(value)
    onOpenChange?.(value)
  }

  useLayoutEffect(() => {
    if (!open || !button.current) return
    const rect = button.current.getBoundingClientRect()
    setPosition({ top: rect.bottom + 4, right: window.innerWidth - rect.right })
  }, [open])

  // Outside click, Escape and scrolling close it (it is fixed to where it opened).
  useEffect(() => {
    if (!open) return
    const close = () => {
      setOpenState(false)
      onOpenChange?.(false)
    }
    const onPointer = (e: PointerEvent) => {
      const target = e.target as Node
      if (!panel.current?.contains(target) && !button.current?.contains(target)) close()
    }
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && close()
    document.addEventListener('pointerdown', onPointer)
    document.addEventListener('keydown', onKey)
    window.addEventListener('scroll', close, true)
    return () => {
      document.removeEventListener('pointerdown', onPointer)
      document.removeEventListener('keydown', onKey)
      window.removeEventListener('scroll', close, true)
    }
  }, [open, onOpenChange])

  const run = (action: () => void) => () => {
    action()
    setOpen(false)
  }

  return (
    <>
      <button
        ref={button}
        type="button"
        aria-label={`Weitere Aktionen für ${card.title}`}
        aria-expanded={open}
        title="Weitere Aktionen"
        onClick={() => setOpen(!open)}
        className="flex size-8 items-center justify-center rounded-full text-white/90 transition-colors hover:bg-black/50 hover:text-white"
      >
        <EllipsisVertical className="size-5" aria-hidden="true" />
      </button>
      {open &&
        position &&
        createPortal(
          <div
            ref={panel}
            role="menu"
            aria-label={`Aktionen für ${card.title}`}
            className="fixed z-50 w-60 overflow-hidden rounded-lg border border-line bg-surface-2 py-1 shadow-2xl"
            style={{ top: position.top, right: position.right }}
          >
            {PLAYABLE.includes(card.kind) && (
              <Link to="/play/$id" params={{ id: String(card.id) }} role="menuitem" className={item}>
                Abspielen
              </Link>
            )}
            <button type="button" role="menuitem" className={item} onClick={run(() => setPlayed.mutate({ id: card.id, played: !card.played }))}>
              {card.played ? 'Als ungesehen markieren' : 'Als gesehen markieren'}
            </button>
            <button type="button" role="menuitem" className={item} onClick={run(() => setPlaylist(true))}>
              Zur Playlist hinzufügen …
            </button>
            {isAdmin && (
              <button type="button" role="menuitem" className={item} onClick={run(() => refresh.mutate(card.id))}>
                Metadaten aktualisieren
              </button>
            )}
          </div>,
          document.body,
        )}
      {playlist && <AddToPlaylistDialog item={card} onClose={() => setPlaylist(false)} />}
    </>
  )
}
