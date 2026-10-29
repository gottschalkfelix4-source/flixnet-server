import { Link } from '@tanstack/react-router'
import { Compass } from 'lucide-react'
import { EmptyState } from '@/components/ui/EmptyState'

export function NotFoundPage() {
  return (
    <div className="pt-16 pb-16">
      <EmptyState icon={Compass} title="Seite nicht gefunden">
        <Link to="/" className="font-semibold text-accent hover:text-accent-hover">
          Zur Startseite
        </Link>
      </EmptyState>
    </div>
  )
}
