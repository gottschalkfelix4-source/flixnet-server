import { Construction } from 'lucide-react'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'

/** Stand-in for sections that later milestones fill in. */
export function PlaceholderPage({ title, note }: { title: string; note: string }) {
  return (
    <div className="pb-16">
      <PageHeader title={title} />
      <EmptyState icon={Construction} title="Noch im Aufbau">
        {note}
      </EmptyState>
    </div>
  )
}
