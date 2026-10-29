import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'

interface EmptyStateProps {
  icon: LucideIcon
  title: string
  children?: ReactNode
}

export function EmptyState({ icon: Icon, title, children }: EmptyStateProps) {
  return (
    <div className="mx-6 flex flex-col items-center rounded-[var(--radius-card)] border border-dashed border-line bg-surface/50 px-6 py-16 text-center md:mx-10">
      <Icon className="mb-4 size-10 text-subtle" aria-hidden="true" />
      <h2 className="text-lg font-bold">{title}</h2>
      {children && <div className="mt-2 max-w-md text-sm text-muted">{children}</div>}
    </div>
  )
}
