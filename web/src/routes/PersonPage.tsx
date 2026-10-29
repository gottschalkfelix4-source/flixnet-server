import { useParams } from '@tanstack/react-router'
import { User } from 'lucide-react'
import { usePerson } from '@/api/queries/items'
import { PosterCard } from '@/components/media/PosterCard'
import { NotFoundPage } from './NotFoundPage'

const ROLE_LABELS: Record<string, string> = {
  actor: 'Darsteller',
  director: 'Regie',
  writer: 'Drehbuch',
  creator: 'Idee',
  producer: 'Produktion',
}

/** A person with everything they worked on in the libraries the user may see. */
export function PersonPage() {
  const { id } = useParams({ from: '/app/person/$id' })
  const person = usePerson(Number(id))

  if (person.isError) return <NotFoundPage />
  if (!person.data) return <div className="h-72 animate-pulse bg-surface/40" />
  const p = person.data

  // One card per item, collecting the roles (e.g. director and writer).
  const byItem = new Map<number, { item: (typeof p.credits)[number]['item']; roles: string[] }>()
  for (const credit of p.credits) {
    const role = credit.role === 'actor' && credit.character ? credit.character : (ROLE_LABELS[credit.role] ?? credit.role)
    const entry = byItem.get(credit.item.id)
    if (entry) entry.roles.push(role)
    else byItem.set(credit.item.id, { item: credit.item, roles: [role] })
  }

  return (
    <div className="px-6 pt-10 pb-16 md:px-10">
      <header className="mb-10 flex items-center gap-6">
        <span className="flex size-32 shrink-0 items-center justify-center overflow-hidden rounded-full bg-surface-2 shadow-xl">
          {p.image_url ? (
            <img src={p.image_url} alt="" className="size-full object-cover" />
          ) : (
            <User className="size-12 text-subtle" aria-hidden="true" />
          )}
        </span>
        <div>
          <h1 className="text-4xl font-extrabold tracking-tight">{p.name}</h1>
          <p className="mt-1 text-sm text-muted">
            {byItem.size} Titel in deinen Bibliotheken
          </p>
        </div>
      </header>
      <div className="grid gap-4" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(150px, 1fr))' }} data-testid="person-credits">
        {[...byItem.values()].map(({ item, roles }) => (
          <div key={item.id}>
            <PosterCard item={item} />
            <p className="mt-0.5 line-clamp-1 text-xs text-subtle">{roles.join(', ')}</p>
          </div>
        ))}
      </div>
    </div>
  )
}
