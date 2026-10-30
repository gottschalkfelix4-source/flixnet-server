import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Heart, ListPlus, Mic, Pause, Play } from 'lucide-react'
import { type Track, useSetTrackFavorite } from '@/api/queries/music'
import { formatTimecode } from '@/lib/format'
import { musicPlayer } from '@/lib/music/player'
import { usePlayerSelect } from '@/lib/music/usePlayer'

interface TrackListProps {
  tracks: Track[]
  /** The tracks the queue gets when one is played (default: `tracks`). */
  queue?: Track[]
  /** Show the album (lists across albums) instead of the track number. */
  showAlbum?: boolean
  /** Hide the artist column when it is the album artist anyway. */
  albumArtist?: string | null
}

/** Rows of tracks: click plays from there, with favorite and "Als Nächstes". */
export function TrackList({ tracks, queue, showAlbum = false, albumArtist }: TrackListProps) {
  const currentId = usePlayerSelect((s) => s.queue[s.index]?.id ?? null)
  const playing = usePlayerSelect((s) => s.playing)
  const favorite = useSetTrackFavorite()
  const all = queue ?? tracks

  return (
    <ol className="divide-y divide-line/40" data-testid="track-list">
      {tracks.map((track, i) => {
        const isCurrent = track.id === currentId
        const showArtist = track.artist && track.artist !== albumArtist
        return (
          <li
            key={track.id}
            data-testid="track-row"
            data-current={isCurrent || undefined}
            className={clsx(
              'group flex items-center gap-3 rounded-lg px-2 py-2 hover:bg-surface-2/70',
              isCurrent && 'bg-surface-2/60',
            )}
          >
            <button
              type="button"
              aria-label={`${track.title} abspielen`}
              onClick={() => {
                if (isCurrent) musicPlayer.toggle()
                else musicPlayer.playTracks(all, Math.max(0, all.findIndex((t) => t.id === track.id)))
              }}
              className="flex size-8 shrink-0 items-center justify-center text-sm text-subtle tabular-nums"
            >
              <span className={clsx('group-hover:hidden', isCurrent && 'text-accent')}>
                {isCurrent && playing ? '♪' : showAlbum ? i + 1 : (track.track_number ?? i + 1)}
              </span>
              {isCurrent && playing ? (
                <Pause className="hidden size-4 text-fg group-hover:block" aria-hidden="true" />
              ) : (
                <Play className="hidden size-4 text-fg group-hover:block" aria-hidden="true" />
              )}
            </button>
            {showAlbum && track.cover_url && (
              <img src={track.cover_url} alt="" className="size-10 shrink-0 rounded object-cover" loading="lazy" />
            )}
            <div className="min-w-0 flex-1">
              <p className={clsx('truncate text-sm font-semibold', isCurrent && 'text-accent')}>{track.title}</p>
              {(showArtist || showAlbum) && (
                <p className="truncate text-xs text-subtle">
                  {track.artist}
                  {showAlbum && track.album_id != null && (
                    <>
                      {' · '}
                      <Link
                        to="/music/album/$id"
                        params={{ id: String(track.album_id) }}
                        className="hover:text-fg hover:underline"
                      >
                        {track.album_title}
                      </Link>
                    </>
                  )}
                </p>
              )}
            </div>
            {track.has_lyrics && <Mic className="size-3.5 shrink-0 text-subtle" aria-label="Liedtext vorhanden" />}
            {track.play_count > 0 && (
              <span className="hidden w-20 shrink-0 text-right text-xs text-subtle md:block">
                {track.play_count}× gehört
              </span>
            )}
            <button
              type="button"
              aria-label="Als Nächstes abspielen"
              title="Als Nächstes abspielen"
              onClick={() => musicPlayer.enqueue([track], true)}
              className="hidden shrink-0 text-subtle hover:text-fg group-hover:block"
            >
              <ListPlus className="size-4" />
            </button>
            <button
              type="button"
              aria-label={track.favorite ? 'Aus Favoriten entfernen' : 'Zu Favoriten'}
              aria-pressed={track.favorite}
              onClick={() => favorite.mutate({ id: track.id, favorite: !track.favorite })}
              className={clsx('shrink-0', track.favorite ? 'text-accent' : 'text-subtle hover:text-fg')}
            >
              <Heart className="size-4" fill={track.favorite ? 'currentColor' : 'none'} />
            </button>
            <span className="w-12 shrink-0 text-right text-xs text-subtle tabular-nums">
              {track.duration_ms ? formatTimecode(track.duration_ms) : ''}
            </span>
          </li>
        )
      })}
    </ol>
  )
}
