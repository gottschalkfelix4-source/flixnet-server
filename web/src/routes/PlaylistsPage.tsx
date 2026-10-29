import { Link, useNavigate } from '@tanstack/react-router'
import { ListVideo, Plus, Sparkles, Users } from 'lucide-react'
import { useState } from 'react'
import { type Playlist, usePlaylists } from '@/api/queries/playlists'
import { PlaylistDialog } from '@/components/media/PlaylistDialog'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'

/** The user's playlists and those other profiles share. */
export function PlaylistsPage() {
  const playlists = usePlaylists()
  const navigate = useNavigate()
  const [creating, setCreating] = useState<'plain' | 'smart' | null>(null)
  const list = playlists.data ?? []

  return (
    <div className="pb-16">
      <PageHeader title="Playlists">
        <div className="flex gap-2">
          <Button variant="ghost" className="h-10 border border-line" onClick={() => setCreating('smart')}>
            <Sparkles className="size-4" aria-hidden="true" />
            Neue Smart-Playlist
          </Button>
          <Button variant="accent" className="h-10" onClick={() => setCreating('plain')}>
            <Plus className="size-4" aria-hidden="true" />
            Neue Playlist
          </Button>
        </div>
      </PageHeader>
      {playlists.data && list.length === 0 ? (
        <EmptyState icon={ListVideo} title="Noch keine Playlists">
          Lege eine Playlist an und füge Filme oder Folgen über „Zur Playlist hinzufügen“ hinzu – oder lass eine
          Smart-Playlist sich selbst füllen.
        </EmptyState>
      ) : (
        <ul
          className="grid gap-5 px-6 md:px-10"
          style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))' }}
          data-testid="playlist-grid"
        >
          {list.map((p) => (
            <li key={p.id}>
              <PlaylistCard playlist={p} />
            </li>
          ))}
        </ul>
      )}
      {creating && (
        <PlaylistDialog
          smart={creating === 'smart'}
          onClose={() => setCreating(null)}
          onSaved={(p) => void navigate({ to: '/playlist/$id', params: { id: String(p.id) } })}
        />
      )}
    </div>
  )
}

function PlaylistCard({ playlist: p }: { playlist: Playlist }) {
  const posters = p.poster_urls.slice(0, 4)
  return (
    <Link to="/playlist/$id" params={{ id: String(p.id) }} className="group block focus-visible:outline-none" data-testid="playlist-card">
      <div className="relative grid aspect-square grid-cols-2 grid-rows-2 overflow-hidden rounded-[var(--radius-card)] bg-surface-2 shadow-[var(--shadow-card)] ring-accent transition group-hover:-translate-y-0.5 group-hover:ring-2 group-focus-visible:ring-2">
        {posters.length === 0 ? (
          <div className="col-span-2 row-span-2 flex items-center justify-center">
            {p.smart ? <Sparkles className="size-10 text-subtle" aria-hidden="true" /> : <ListVideo className="size-10 text-subtle" aria-hidden="true" />}
          </div>
        ) : (
          posters.map((url, i) => (
            <img
              key={url}
              src={url}
              alt=""
              loading="lazy"
              className={`size-full object-cover ${posters.length === 1 ? 'col-span-2 row-span-2' : posters.length === 2 ? 'row-span-2' : posters.length === 3 && i === 0 ? 'row-span-2' : ''}`}
            />
          ))
        )}
        {p.smart && (
          <span className="absolute top-2 left-2 inline-flex items-center gap-1 rounded-full bg-black/70 px-2 py-0.5 text-[11px] font-bold">
            <Sparkles className="size-3" aria-hidden="true" />
            Smart
          </span>
        )}
      </div>
      <div className="mt-2 px-0.5">
        <p className="truncate text-sm font-semibold group-hover:text-white">{p.name}</p>
        <p className="flex items-center gap-1 truncate text-xs text-subtle">
          {p.item_count} Titel
          {p.shared && (
            <>
              {' · '}
              <Users className="size-3" aria-hidden="true" />
              {p.is_owner ? 'geteilt' : `von ${p.owner_name}`}
            </>
          )}
        </p>
      </div>
    </Link>
  )
}
