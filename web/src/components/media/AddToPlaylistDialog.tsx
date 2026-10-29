import clsx from 'clsx'
import { Check, ListPlus, ListVideo, Plus } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useAddToPlaylist, useCreatePlaylist, usePlaylists } from '@/api/queries/playlists'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'

interface Target {
  id: number
  title: string
  kind: string
}

/** Pick one of the user's playlists (or create one) and add the item to it. */
export function AddToPlaylistDialog({ item, onClose }: { item: Target; onClose: () => void }) {
  const playlists = usePlaylists()
  const add = useAddToPlaylist()
  const create = useCreatePlaylist()
  const [name, setName] = useState('')
  const [done, setDone] = useState<string | null>(null)
  const own = playlists.data?.filter((p) => p.is_owner && !p.smart) ?? []
  const expands = item.kind === 'series' || item.kind === 'season'

  function addTo(id: number, playlistName: string) {
    add.mutate({ id, itemIds: [item.id] }, { onSuccess: () => setDone(playlistName) })
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    const trimmed = name.trim()
    if (!trimmed) return
    create.mutate({ name: trimmed }, { onSuccess: (created) => addTo(created.id, created.name) })
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title="Zur Playlist hinzufügen"
      description={expands ? `„${item.title}“ – alle Folgen werden hinzugefügt.` : `„${item.title}“`}
    >
      {done ? (
        <div className="space-y-4">
          <p className="flex items-center gap-2 text-sm" role="status">
            <Check className="size-4 text-accent" aria-hidden="true" />
            Zu „{done}“ hinzugefügt.
          </p>
          <div className="flex justify-end">
            <Button onClick={onClose}>Fertig</Button>
          </div>
        </div>
      ) : (
        <div className="space-y-4">
          <FormError error={add.error ?? create.error} />
          {own.length > 0 && (
            <ul className="max-h-64 space-y-1 overflow-y-auto" aria-label="Deine Playlists">
              {own.map((p) => (
                <li key={p.id}>
                  <button
                    type="button"
                    disabled={add.isPending}
                    onClick={() => addTo(p.id, p.name)}
                    className="flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm hover:bg-surface-2"
                  >
                    <ListVideo className="size-4 text-subtle" aria-hidden="true" />
                    <span className="min-w-0 flex-1 truncate font-semibold">{p.name}</span>
                    <span className="text-xs text-subtle">{p.item_count}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <form onSubmit={submit} className="flex gap-2">
            <input
              aria-label="Name der neuen Playlist"
              placeholder="Neue Playlist"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="h-10 min-w-0 flex-1 rounded-lg border border-line bg-surface-2 px-3 text-sm focus:border-accent focus:outline-none"
            />
            <Button type="submit" variant="accent" className="h-10 px-4" loading={create.isPending} disabled={!name.trim()}>
              <Plus className="size-4" aria-hidden="true" />
              Anlegen
            </Button>
          </form>
        </div>
      )}
    </Dialog>
  )
}

/** Round icon button that opens [`AddToPlaylistDialog`]. */
export function AddToPlaylistButton({ item, overlay = false }: { item: Target; overlay?: boolean }) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button
        type="button"
        aria-label="Zur Playlist hinzufügen"
        title="Zur Playlist hinzufügen"
        onClick={(e) => {
          e.preventDefault()
          e.stopPropagation()
          setOpen(true)
        }}
        className={clsx(
          'flex items-center justify-center rounded-full backdrop-blur transition',
          overlay
            ? 'size-8 bg-black/60 text-fg opacity-0 group-hover/card:opacity-100 hover:bg-black/80 focus-visible:opacity-100'
            : 'size-11 bg-white/15 hover:bg-white/25',
        )}
      >
        <ListPlus className={overlay ? 'size-4' : 'size-5'} />
      </button>
      {open && <AddToPlaylistDialog item={item} onClose={() => setOpen(false)} />}
    </>
  )
}
