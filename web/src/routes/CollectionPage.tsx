import { useNavigate, useParams } from '@tanstack/react-router'
import { Pencil, Trash2, X } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import {
  type CollectionDetail,
  useCollection,
  useDeleteCollection,
  useRemoveFromCollection,
  useUpdateCollection,
} from '@/api/queries/collections'
import { FormError } from '@/components/layout/AuthLayout'
import { BlurImage } from '@/components/media/BlurImage'
import { PosterCard } from '@/components/media/PosterCard'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { TextField } from '@/components/ui/TextField'
import { useSession } from '@/lib/session'
import { NotFoundPage } from './NotFoundPage'

/** A collection: backdrop, description and its members in release order. */
export function CollectionPage() {
  const { id } = useParams({ from: '/app/collection/$id' })
  const collection = useCollection(Number(id))
  const session = useSession()
  const navigate = useNavigate()
  const remove = useRemoveFromCollection()
  const deleteCollection = useDeleteCollection()
  const [editing, setEditing] = useState(false)
  const [confirming, setConfirming] = useState(false)

  if (collection.isError) return <NotFoundPage />
  if (!collection.data) return <div className="h-[50vh] animate-pulse bg-surface/40" />
  const c = collection.data
  const editable = !!session?.user.is_admin && c.kind === 'manual'

  // Two clicks, like removing a library: the titles themselves stay.
  function onDelete() {
    if (!confirming) return setConfirming(true)
    deleteCollection.mutate(c.id, { onSuccess: () => void navigate({ to: '/collections' }) })
  }

  return (
    <div key={c.id} className="pb-16">
      <section className="relative isolate min-h-[45vh]" data-testid="collection-header">
        <div aria-hidden="true" className="absolute inset-0 -z-10 overflow-hidden">
          <BlurImage src={c.backdrop_url} blurhash={c.backdrop_blurhash} alt="" eager className="size-full brightness-75" />
          <div className="absolute inset-0 bg-gradient-to-t from-canvas via-canvas/70 to-canvas/10" />
          <div className="absolute inset-0 bg-gradient-to-r from-canvas/95 via-canvas/50 to-transparent" />
        </div>
        <div className="flex min-h-[45vh] flex-col justify-end gap-4 px-6 pt-20 pb-8 md:px-10">
          <p className="text-xs font-bold tracking-widest text-accent uppercase">
            {c.kind === 'auto' ? 'Filmreihe' : c.kind === 'theme' ? 'Thema' : 'Sammlung'}
          </p>
          <h1 className="text-4xl font-extrabold tracking-tight md:text-5xl">{c.name}</h1>
          <p className="text-sm text-fg/85" data-testid="collection-counts">
            {c.item_count} Titel{c.played_count > 0 && ` · ${c.played_count} gesehen`}
          </p>
          {c.overview && <p className="max-w-2xl text-base leading-relaxed text-fg/80">{c.overview}</p>}
          {editable && (
            <div className="flex gap-2 pt-1">
              <Button variant="ghost" className="h-10 bg-white/10" onClick={() => setEditing(true)}>
                <Pencil className="size-4" aria-hidden="true" />
                Bearbeiten
              </Button>
              <Button
                variant="ghost"
                className={confirming ? 'h-10 bg-red-500 text-white hover:bg-red-400 hover:text-white' : 'h-10 bg-white/10'}
                onClick={onDelete}
                loading={deleteCollection.isPending}
              >
                <Trash2 className="size-4" aria-hidden="true" />
                {confirming ? 'Wirklich löschen?' : 'Sammlung löschen'}
              </Button>
            </div>
          )}
        </div>
      </section>

      {c.items.length === 0 ? (
        <p className="px-6 text-sm text-muted md:px-10">
          Noch leer. Füge Filme oder Serien auf ihrer Detailseite über „Zur Sammlung hinzufügen“ hinzu.
        </p>
      ) : (
        <div
          className="grid gap-4 px-6 md:px-10"
          style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(150px, 1fr))' }}
          data-testid="collection-items"
        >
          {c.items.map((item) => (
            <div key={item.id}>
              <PosterCard item={item} />
              {editable && (
                <button
                  type="button"
                  aria-label={`${item.title} aus der Sammlung entfernen`}
                  onClick={() => remove.mutate({ id: c.id, itemId: item.id })}
                  className="mt-1 inline-flex items-center gap-1 rounded px-0.5 text-xs text-subtle hover:text-fg"
                >
                  <X className="size-3.5" aria-hidden="true" />
                  Entfernen
                </button>
              )}
            </div>
          ))}
        </div>
      )}
      {editing && <EditCollectionDialog collection={c} onClose={() => setEditing(false)} />}
    </div>
  )
}

function EditCollectionDialog({ collection, onClose }: { collection: CollectionDetail; onClose: () => void }) {
  const update = useUpdateCollection()
  const [name, setName] = useState(collection.name)
  const [overview, setOverview] = useState(collection.overview ?? '')

  function submit(event: FormEvent) {
    event.preventDefault()
    update.mutate({ id: collection.id, name: name.trim(), overview: overview.trim() || null }, { onSuccess: onClose })
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()} title="Sammlung bearbeiten">
      <form onSubmit={submit} className="space-y-4">
        <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} required maxLength={200} />
        <label className="flex flex-col gap-1.5">
          <span className="text-sm font-semibold text-muted">Beschreibung</span>
          <textarea
            value={overview}
            onChange={(e) => setOverview(e.target.value)}
            rows={3}
            className="rounded-lg border border-line bg-surface-2 px-3.5 py-2 text-fg focus:border-accent focus:outline-none"
          />
        </label>
        <FormError error={update.error} />
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Abbrechen
          </Button>
          <Button type="submit" variant="accent" loading={update.isPending} disabled={!name.trim()}>
            Speichern
          </Button>
        </div>
      </form>
    </Dialog>
  )
}
