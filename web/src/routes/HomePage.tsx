import { Link } from '@tanstack/react-router'
import { Library } from 'lucide-react'
import { useItems } from '@/api/queries/items'
import { useLibraries } from '@/api/queries/libraries'
import { EmptyState } from '@/components/ui/EmptyState'
import { Hero } from '@/components/media/Hero'
import { Row } from '@/components/media/Row'
import { useSession } from '@/lib/session'

export function HomePage() {
  const session = useSession()
  const libraries = useLibraries()
  const featured = useItems({ kind: 'movie,series', sort: 'date_added', order: 'desc', limit: 12 })
  const continueWatching = useItems({ kind: 'movie,episode', sort: 'date_added', order: 'desc', limit: 20 })
  const newMovies = useItems({ kind: 'movie', sort: 'date_added', order: 'desc', limit: 20 })
  const newSeries = useItems({ kind: 'series', sort: 'date_added', order: 'desc', limit: 20 })
  const favorites = useItems({ kind: 'movie,series', favorite: true, limit: 20 })
  const topRated = useItems({ kind: 'movie,series', sort: 'rating', order: 'desc', limit: 20 })

  if (libraries.data?.length === 0) {
    return (
      <div className="pt-16 pb-16">
        <EmptyState icon={Library} title="Noch keine Bibliotheken">
          {session?.user.is_admin ? (
            <>
              Füge deinen ersten Medienordner hinzu:{' '}
              <Link to="/settings" search={{ tab: 'libraries' }} className="font-semibold text-accent hover:text-accent-hover">
                Bibliothek hinzufügen
              </Link>
            </>
          ) : (
            'Sobald ein Administrator Bibliotheken anlegt, erscheinen sie hier.'
          )}
        </EmptyState>
      </div>
    )
  }

  const heroItems = (featured.data?.items ?? []).filter((item) => item.backdrop_url).slice(0, 6)
  const resume = continueWatching.data?.items.filter((i) => i.position_ms > 0 && !i.played)

  return (
    <div className="pb-16">
      {heroItems.length > 0 ? <Hero items={heroItems} /> : <div className="h-16" />}
      <div className={heroItems.length > 0 ? '-mt-8 relative' : ''}>
        <Row title="Weiterschauen" items={resume} wide />
        <Row title="Neu: Filme" items={newMovies.data?.items} loading={newMovies.isPending} />
        <Row title="Neu: Serien" items={newSeries.data?.items} loading={newSeries.isPending} />
        <Row title="Favoriten" items={favorites.data?.items} />
        <Row title="Am besten bewertet" items={topRated.data?.items.filter((i) => i.community_rating != null)} />
      </div>
    </div>
  )
}
