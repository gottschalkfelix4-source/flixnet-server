import { Link, useNavigate, useParams } from '@tanstack/react-router'
import clsx from 'clsx'
import { ArrowDown, ArrowUp, Check, GripVertical, Pencil, Play, Sparkles, Trash2, Users, X } from 'lucide-react'
import { useState } from 'react'
import {
  type PlaylistDetail,
  useDeletePlaylist,
  usePlaylist,
  useRemoveFromPlaylist,
  useReorderPlaylist,
} from '@/api/queries/playlists'
import { BlurImage } from '@/components/media/BlurImage'
import { PlaylistDialog } from '@/components/media/PlaylistDialog'
import { Button } from '@/components/ui/Button'
import { episodeLabel, formatRuntime } from '@/lib/media'
import { NotFoundPage } from './NotFoundPage'

type Entry = PlaylistDetail['entries'][number]

/** A playlist: its items in order, reorderable by drag or buttons (owner only). */
export function PlaylistPage() {
  const { id } = useParams({ from: '/app/playlist/$id' })
  const playlist = usePlaylist(Number(id))
  const navigate = useNavigate()
  const remove = useRemoveFromPlaylist()
  const reorder = useReorderPlaylist()
  const deletePlaylist = useDeletePlaylist()
  const [editing, setEditing] = useState(false)
  const [confirming, setConfirming] = useState(false)
  const [dragged, setDragged] = useState<number | null>(null)

  if (playlist.isError) return <NotFoundPage />
  if (!playlist.data) return <div className="h-72 animate-pulse bg-surface/40" />
  const p = playlist.data
  const editable = p.is_owner && !p.smart
  const firstPlayable = p.entries.find((e) => e.item.kind === 'movie' || e.item.kind === 'episode')

  function move(from: number, to: number) {
    if (to < 0 || to >= p.entries.length || from === to) return
    const ids = p.entries.map((e) => e.entry_id!)
    const [moved] = ids.splice(from, 1)
    ids.splice(to, 0, moved!)
    reorder.mutate({ id: p.id, entryIds: ids })
  }

  function onDelete() {
    if (!confirming) return setConfirming(true)
    deletePlaylist.mutate(p.id, { onSuccess: () => void navigate({ to: '/playlists' }) })
  }

  return (
    <div key={p.id} className="px-6 pt-10 pb-16 md:px-10">
      <header className="mb-8 flex flex-wrap items-end justify-between gap-4">
        <div>
          <p className="flex items-center gap-1.5 text-xs font-bold tracking-widest text-accent uppercase">
            {p.smart ? (
              <>
                <Sparkles className="size-3.5" aria-hidden="true" /> Smart-Playlist
              </>
            ) : (
              'Playlist'
            )}
          </p>
          <h1 className="mt-1 text-3xl font-extrabold tracking-tight md:text-4xl">{p.name}</h1>
          <p className="mt-1 flex items-center gap-1.5 text-sm text-muted" data-testid="playlist-meta">
            {p.item_count} Titel
            {p.shared && (
              <>
                {' · '}
                <Users className="size-3.5" aria-hidden="true" />
                {p.is_owner ? 'Für alle Profile sichtbar' : `Geteilt von ${p.owner_name}`}
              </>
            )}
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          {firstPlayable && (
            <Link
              to="/play/$id"
              params={{ id: String(firstPlayable.item.id) }}
              className="inline-flex h-10 items-center gap-2 rounded-full bg-fg px-5 text-sm font-bold text-canvas hover:bg-white"
            >
              <Play className="size-4 fill-current" aria-hidden="true" />
              Abspielen
            </Link>
          )}
          {p.is_owner && (
            <>
              <Button variant="ghost" className="h-10 border border-line" onClick={() => setEditing(true)}>
                <Pencil className="size-4" aria-hidden="true" />
                Bearbeiten
              </Button>
              <Button
                variant="ghost"
                className={clsx('h-10 border border-line', confirming && 'border-red-500 bg-red-500 text-white hover:bg-red-400 hover:text-white')}
                onClick={onDelete}
                loading={deletePlaylist.isPending}
              >
                <Trash2 className="size-4" aria-hidden="true" />
                {confirming ? 'Wirklich löschen?' : 'Löschen'}
              </Button>
            </>
          )}
        </div>
      </header>

      {p.entries.length === 0 ? (
        <p className="text-sm text-muted">
          {p.smart
            ? 'Gerade passt nichts zum Filter.'
            : 'Noch leer. Füge Filme, Serien oder Folgen über „Zur Playlist hinzufügen“ hinzu.'}
        </p>
      ) : (
        <ol className="space-y-2" data-testid="playlist-entries">
          {p.entries.map((entry, index) => (
            <EntryRow
              key={entry.entry_id ?? `smart-${entry.item.id}`}
              entry={entry}
              index={index}
              count={p.entries.length}
              editable={editable}
              dragging={dragged === index}
              onDragStart={() => setDragged(index)}
              onDrop={() => {
                if (dragged !== null) move(dragged, index)
                setDragged(null)
              }}
              onMove={(to) => move(index, to)}
              onRemove={() => remove.mutate({ id: p.id, entryId: entry.entry_id! })}
            />
          ))}
        </ol>
      )}
      {editing && <PlaylistDialog playlist={p} onClose={() => setEditing(false)} />}
    </div>
  )
}

interface EntryRowProps {
  entry: Entry
  index: number
  count: number
  editable: boolean
  dragging: boolean
  onDragStart: () => void
  onDrop: () => void
  onMove: (to: number) => void
  onRemove: () => void
}

function EntryRow({ entry, index, count, editable, dragging, onDragStart, onDrop, onMove, onRemove }: EntryRowProps) {
  const item = entry.item
  const label = episodeLabel(item)
  const runtime = formatRuntime(item.runtime_ms)
  const image = item.kind === 'episode' ? item.poster_url : (item.backdrop_url ?? item.poster_url)
  return (
    <li
      draggable={editable}
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = 'move'
        onDragStart()
      }}
      onDragOver={(e) => editable && e.preventDefault()}
      onDrop={(e) => {
        e.preventDefault()
        onDrop()
      }}
      className={clsx(
        'flex items-center gap-3 rounded-[var(--radius-card)] p-2 transition-colors hover:bg-surface-2',
        dragging && 'opacity-50',
      )}
      data-testid="playlist-entry"
    >
      {editable && <GripVertical className="size-4 shrink-0 cursor-grab text-subtle" aria-hidden="true" />}
      <span className="w-6 shrink-0 text-right text-sm font-semibold text-subtle tabular-nums">{index + 1}</span>
      <Link to="/item/$id" params={{ id: String(item.id) }} className="group flex min-w-0 flex-1 items-center gap-4">
        <div className="relative w-28 shrink-0 overflow-hidden rounded-lg md:w-36">
          <BlurImage src={image} blurhash={item.kind === 'episode' ? item.poster_blurhash : null} alt="" className="aspect-video" />
          {item.played && (
            <span className="absolute top-1 right-1 flex size-5 items-center justify-center rounded-full bg-accent text-accent-fg">
              <Check className="size-3.5" strokeWidth={3} aria-label="Gesehen" />
            </span>
          )}
        </div>
        <div className="min-w-0">
          <p className="truncate font-bold group-hover:text-white" data-testid="playlist-entry-title">
            {item.title}
          </p>
          <p className="truncate text-xs text-subtle">{[label, item.year, runtime].filter(Boolean).join(' · ')}</p>
        </div>
      </Link>
      {editable && (
        <div className="flex shrink-0 items-center gap-1">
          <IconButton label={`${item.title} nach oben`} disabled={index === 0} onClick={() => onMove(index - 1)}>
            <ArrowUp className="size-4" />
          </IconButton>
          <IconButton label={`${item.title} nach unten`} disabled={index === count - 1} onClick={() => onMove(index + 1)}>
            <ArrowDown className="size-4" />
          </IconButton>
          <IconButton label={`${item.title} entfernen`} onClick={onRemove}>
            <X className="size-4" />
          </IconButton>
        </div>
      )}
    </li>
  )
}

function IconButton({ label, disabled, onClick, children }: { label: string; disabled?: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className="flex size-9 items-center justify-center rounded-full text-muted hover:bg-surface-3 hover:text-fg disabled:opacity-30"
    >
      {children}
    </button>
  )
}
