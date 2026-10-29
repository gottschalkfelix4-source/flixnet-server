import { Link, useNavigate } from '@tanstack/react-router'
import { Check, Layers, Plus } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { type Collection, useCollections, useCreateCollection } from '@/api/queries/collections'
import { FormError } from '@/components/layout/AuthLayout'
import { BlurImage } from '@/components/media/BlurImage'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'
import { TextField } from '@/components/ui/TextField'
import { useSession } from '@/lib/session'

function countLabel(c: Pick<Collection, 'item_count' | 'played_count'>): string {
  return c.played_count > 0 ? `${c.item_count} Titel · ${c.played_count} gesehen` : `${c.item_count} Titel`
}

/** All collections with at least one title the user may see. */
export function CollectionsPage() {
  const session = useSession()
  const collections = useCollections()
  const [creating, setCreating] = useState(false)
  const list = collections.data ?? []

  return (
    <div className="pb-16">
      <PageHeader title="Sammlungen">
        {session?.user.is_admin && (
          <Button variant="accent" className="h-10" onClick={() => setCreating(true)}>
            <Plus className="size-4" aria-hidden="true" />
            Sammlung anlegen
          </Button>
        )}
      </PageHeader>
      {collections.data && list.length === 0 ? (
        <EmptyState icon={Layers} title="Noch keine Sammlungen">
          Filmreihen erscheinen automatisch, sobald Filme mit TMDB abgeglichen sind.
          {session?.user.is_admin && ' Eigene Sammlungen legst du hier an.'}
        </EmptyState>
      ) : (
        <div
          className="grid gap-4 px-6 md:px-10"
          style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(150px, 1fr))' }}
          data-testid="collection-grid"
        >
          {list.map((c) => (
            <CollectionCard key={c.id} collection={c} />
          ))}
        </div>
      )}
      {creating && <CreateCollectionDialog onClose={() => setCreating(false)} />}
    </div>
  )
}

function CollectionCard({ collection: c }: { collection: Collection }) {
  const complete = c.item_count > 0 && c.played_count === c.item_count
  return (
    <Link
      to="/collection/$id"
      params={{ id: String(c.id) }}
      className="group block min-w-0 focus-visible:outline-none"
      data-testid="collection-card"
    >
      <div className="relative aspect-[2/3] overflow-hidden rounded-[var(--radius-card)] shadow-[var(--shadow-card)] ring-accent transition duration-200 group-hover:-translate-y-0.5 group-hover:ring-2 group-focus-visible:ring-2">
        <BlurImage src={c.poster_url} blurhash={c.poster_blurhash} alt={c.name} className="absolute inset-0" />
        {!c.poster_url && (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-3 text-center">
            <Layers className="size-7 text-subtle" aria-hidden="true" />
            <span className="line-clamp-3 text-sm font-semibold text-muted">{c.name}</span>
          </div>
        )}
        {complete && (
          <span className="absolute top-2 right-2 flex size-6 items-center justify-center rounded-full bg-accent text-accent-fg shadow" title="Alles gesehen">
            <Check className="size-4" strokeWidth={3} aria-label="Alles gesehen" />
          </span>
        )}
      </div>
      <div className="mt-2 px-0.5">
        <p className="truncate text-sm font-semibold text-fg group-hover:text-white">{c.name}</p>
        <p className="truncate text-xs text-subtle">{countLabel(c)}</p>
      </div>
    </Link>
  )
}

function CreateCollectionDialog({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate()
  const create = useCreateCollection()
  const [name, setName] = useState('')
  const [overview, setOverview] = useState('')

  function submit(event: FormEvent) {
    event.preventDefault()
    create.mutate(
      { name: name.trim(), overview: overview.trim() || null },
      { onSuccess: (c) => void navigate({ to: '/collection/$id', params: { id: String(c.id) } }) },
    )
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()} title="Sammlung anlegen" description="Filme und Serien fügst du danach auf ihren Seiten hinzu.">
      <form onSubmit={submit} className="space-y-4">
        <TextField label="Name" value={name} onChange={(e) => setName(e.target.value)} required maxLength={200} autoFocus />
        <label className="flex flex-col gap-1.5">
          <span className="text-sm font-semibold text-muted">Beschreibung</span>
          <textarea
            value={overview}
            onChange={(e) => setOverview(e.target.value)}
            rows={3}
            className="rounded-lg border border-line bg-surface-2 px-3.5 py-2 text-fg focus:border-accent focus:outline-none"
          />
        </label>
        <FormError error={create.error} />
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            Abbrechen
          </Button>
          <Button type="submit" variant="accent" loading={create.isPending} disabled={!name.trim()}>
            Anlegen
          </Button>
        </div>
      </form>
    </Dialog>
  )
}
