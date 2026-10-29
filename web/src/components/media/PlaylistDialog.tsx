import { type FormEvent, useState } from 'react'
import { useGenres } from '@/api/queries/items'
import { type Playlist, type SmartFilter, useCreatePlaylist, useUpdatePlaylist } from '@/api/queries/playlists'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { Checkbox, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

const KINDS = [
  { value: 'movie', label: 'Filme' },
  { value: 'series', label: 'Serien' },
  { value: 'episode', label: 'Folgen' },
]

const SORTS = [
  { value: 'title', label: 'Titel' },
  { value: 'date_added', label: 'Hinzugefügt' },
  { value: 'year', label: 'Erscheinungsjahr' },
  { value: 'premiere_date', label: 'Erscheinungsdatum' },
  { value: 'rating', label: 'Bewertung' },
]

const numberOrNull = (value: string) => (value.trim() === '' ? null : Number(value))

interface PlaylistDialogProps {
  /** Edit this playlist; otherwise create a new one (`smart` picks the kind). */
  playlist?: Playlist
  smart?: boolean
  onClose: () => void
  onSaved?: (playlist: Playlist) => void
}

/** Create or edit a playlist; smart playlists also edit their saved filter. */
export function PlaylistDialog({ playlist, smart = false, onClose, onSaved }: PlaylistDialogProps) {
  const isSmart = playlist ? playlist.smart : smart
  const initial: SmartFilter = playlist?.smart_filter ?? {
    kinds: ['movie'],
    genres: [],
    library_ids: [],
    year_from: null,
    year_to: null,
    played: null,
    favorite: null,
    min_rating: null,
    sort: 'title',
    order: 'asc',
    limit: 100,
  }
  const [name, setName] = useState(playlist?.name ?? '')
  const [shared, setShared] = useState(playlist?.shared ?? false)
  const [kinds, setKinds] = useState<string[]>(initial.kinds ?? [])
  const [genres, setGenres] = useState<string[]>(initial.genres ?? [])
  const [yearFrom, setYearFrom] = useState(initial.year_from?.toString() ?? '')
  const [yearTo, setYearTo] = useState(initial.year_to?.toString() ?? '')
  const [played, setPlayed] = useState(initial.played == null ? '' : initial.played ? 'yes' : 'no')
  const [favorite, setFavorite] = useState(initial.favorite === true)
  const [minRating, setMinRating] = useState(initial.min_rating?.toString() ?? '')
  const [sort, setSort] = useState<NonNullable<SmartFilter['sort']>>(initial.sort ?? 'title')
  const [order, setOrder] = useState<NonNullable<SmartFilter['order']>>(initial.order ?? 'asc')
  const [limit, setLimit] = useState(initial.limit?.toString() ?? '100')
  const genreList = useGenres()
  const create = useCreatePlaylist()
  const update = useUpdatePlaylist()

  const toggle = (list: string[], value: string, on: boolean) =>
    on ? [...list.filter((v) => v !== value), value] : list.filter((v) => v !== value)

  function submit(event: FormEvent) {
    event.preventDefault()
    const filter: SmartFilter | undefined = isSmart
      ? {
          kinds,
          genres,
          library_ids: initial.library_ids,
          year_from: numberOrNull(yearFrom),
          year_to: numberOrNull(yearTo),
          played: played === '' ? null : played === 'yes',
          favorite: favorite ? true : null,
          min_rating: numberOrNull(minRating),
          sort,
          order,
          limit: numberOrNull(limit),
        }
      : undefined
    const done = {
      onSuccess: (saved: Playlist) => {
        onSaved?.(saved)
        onClose()
      },
    }
    if (playlist) update.mutate({ id: playlist.id, name: name.trim(), shared, smart_filter: filter }, done)
    else create.mutate({ name: name.trim(), shared, smart_filter: filter }, done)
  }

  const title = playlist ? 'Playlist bearbeiten' : isSmart ? 'Neue Smart-Playlist' : 'Neue Playlist'
  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={title}
      description={isSmart ? 'Zeigt immer alles, was gerade zum Filter passt.' : undefined}
    >
      <form onSubmit={submit} className="-mr-2 max-h-[65vh] space-y-4 overflow-y-auto pr-2">
        <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} required maxLength={100} autoFocus />
        <Checkbox
          label="Für alle Profile sichtbar"
          hint="Andere Profile im Haushalt sehen die Playlist, können sie aber nicht ändern."
          checked={shared}
          onChange={setShared}
        />
        {isSmart && (
          <fieldset className="space-y-4 rounded-lg border border-line p-4">
            <legend className="px-1 text-sm font-semibold text-muted">Filter</legend>
            <div className="flex flex-wrap gap-4" role="group" aria-label="Typ">
              {KINDS.map((k) => (
                <Checkbox
                  key={k.value}
                  label={k.label}
                  checked={kinds.includes(k.value)}
                  onChange={(on) => setKinds(toggle(kinds, k.value, on))}
                />
              ))}
            </div>
            {(genreList.data?.length ?? 0) > 0 && (
              <div role="group" aria-label="Genres" className="space-y-1.5">
                <p className="text-sm font-semibold text-muted">Genres (eines davon)</p>
                <div className="grid max-h-36 grid-cols-2 gap-2 overflow-y-auto">
                  {genreList.data!.map((g) => (
                    <Checkbox
                      key={g.name}
                      label={g.name}
                      checked={genres.includes(g.name)}
                      onChange={(on) => setGenres(toggle(genres, g.name, on))}
                    />
                  ))}
                </div>
              </div>
            )}
            <div className="grid grid-cols-2 gap-3">
              <TextField label="Jahr von" type="number" inputMode="numeric" value={yearFrom} onChange={(e) => setYearFrom(e.target.value)} />
              <TextField label="Jahr bis" type="number" inputMode="numeric" value={yearTo} onChange={(e) => setYearTo(e.target.value)} />
            </div>
            <SelectField
              label="Gesehen"
              value={played}
              onChange={setPlayed}
              options={[
                { value: '', label: 'Alle' },
                { value: 'no', label: 'Nur ungesehene' },
                { value: 'yes', label: 'Nur gesehene' },
              ]}
            />
            <Checkbox label="Nur Favoriten" checked={favorite} onChange={setFavorite} />
            <TextField
              label="Mindestbewertung (0–10)"
              type="number"
              min={0}
              max={10}
              step={0.5}
              value={minRating}
              onChange={(e) => setMinRating(e.target.value)}
            />
            <div className="grid grid-cols-2 gap-3">
              <SelectField label="Sortierung" value={sort} onChange={(v) => setSort(v as typeof sort)} options={SORTS} />
              <SelectField
                label="Reihenfolge"
                value={order}
                onChange={(v) => setOrder(v as typeof order)}
                options={[
                  { value: 'asc', label: 'Aufsteigend' },
                  { value: 'desc', label: 'Absteigend' },
                ]}
              />
            </div>
            <TextField label="Höchstens (Titel)" type="number" min={1} max={500} value={limit} onChange={(e) => setLimit(e.target.value)} />
          </fieldset>
        )}
        <FormError error={create.error ?? update.error} />
        <div className="flex justify-end gap-2 pt-1">
          <Button type="button" variant="ghost" onClick={onClose}>
            Abbrechen
          </Button>
          <Button type="submit" variant="accent" loading={create.isPending || update.isPending} disabled={!name.trim()}>
            Speichern
          </Button>
        </div>
      </form>
    </Dialog>
  )
}
