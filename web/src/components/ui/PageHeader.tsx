import type { ReactNode } from 'react'

export function PageHeader({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <header className="flex flex-wrap items-end justify-between gap-4 px-6 pt-8 pb-6 md:px-10">
      <h1 className="text-3xl font-extrabold tracking-tight">{title}</h1>
      {children}
    </header>
  )
}
