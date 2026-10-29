import { Link } from '@tanstack/react-router'
import { useItemCollections } from '@/api/queries/collections'
import { Row } from './Row'

/** "Teil der Sammlung …": one row per collection the movie or series belongs to. */
export function CollectionRows({ itemId }: { itemId: number }) {
  const collections = useItemCollections(itemId)
  return (
    <>
      {collections.data?.map((c) => (
        <Row
          key={c.id}
          title={`Teil der Sammlung „${c.name}“`}
          items={c.items}
          action={
            <Link
              to="/collection/$id"
              params={{ id: String(c.id) }}
              className="shrink-0 text-sm font-semibold text-accent hover:text-accent-hover"
            >
              Zur Sammlung
            </Link>
          }
        />
      ))}
    </>
  )
}
