import type { ReactNode } from 'react'

export function PageHeader({ title, children }: { title: string; children?: ReactNode }) {
  return (
    // Right padding keeps the controls clear of the top bar's dashboard/settings buttons.
    <header className="flex flex-wrap items-end justify-between gap-4 pt-8 pr-32 pb-6 pl-6 md:pr-36 md:pl-10">
      <h1 className="text-3xl font-extrabold tracking-tight">{title}</h1>
      {children}
    </header>
  )
}
