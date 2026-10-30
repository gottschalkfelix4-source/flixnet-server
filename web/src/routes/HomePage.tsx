import { Link } from '@tanstack/react-router'
import { Library } from 'lucide-react'
import { useRecommendationRows } from '@/api/queries/ai'
import { useItems } from '@/api/queries/items'
import { useLibraries } from '@/api/queries/libraries'
import { useNextUp, useResumeItems } from '@/api/queries/playstate'
import { EmptyState } from '@/components/ui/EmptyState'
import { Hero } from '@/components/media/Hero'
import { Row } from '@/components/media/Row'
import { useSession } from '@/lib/session'

export function HomePage() {
  const session = useSession()
  const libraries = useLibraries()
  const featured = useItems({ kind: 'movie,series', sort: 'date_added', order: 'desc', limit: 12 })
  const resume = useResumeItems()
  const nextUp = useNextUp()
  const aiRows = useRecommendationRows()
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

  return (
    <div className="pb-16">
      {heroItems.length > 0 ? <Hero items={heroItems} /> : <div className="h-16" />}
      <div className={heroItems.length > 0 ? '-mt-8 relative' : ''}>
        <Row title="Weiterschauen" items={resume.data} wide />
        <Row title="Nächste Folge" items={nextUp.data} wide />
        {aiRows.data?.map((row) => (
          <Row key={`${row.kind}-${row.source_item_id ?? 0}`} title={row.title} items={row.items} />
        ))}
        <Row title="Neu: Filme" items={newMovies.data?.items} loading={newMovies.isPending} />
        <Row title="Neu: Serien" items={newSeries.data?.items} loading={newSeries.isPending} />
        <Row title="Favoriten" items={favorites.data?.items} />
        <Row title="Am besten bewertet" items={topRated.data?.items.filter((i) => i.community_rating != null)} />
      </div>
    </div>
  )
}
