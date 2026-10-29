import type { ReactNode } from 'react'
import { Logo } from './Logo'

/** Centered card on a cinematic backdrop, for setup and sign-in. */
export function AuthLayout({
  title,
  subtitle,
  children,
}: {
  title: string
  subtitle?: string
  children: ReactNode
}) {
  return (
    <div className="relative isolate flex min-h-dvh items-center justify-center px-4 py-12">
      <div
        aria-hidden="true"
        className="absolute inset-0 -z-10 bg-[radial-gradient(800px_500px_at_80%_0%,rgb(255_107_61/0.18),transparent_60%),radial-gradient(700px_500px_at_0%_100%,rgb(120_120_255/0.08),transparent_60%)]"
      />
      <main className="w-full max-w-sm">
        <div className="mb-8 flex justify-center">
          <Logo />
        </div>
        <div className="rounded-2xl border border-line/70 bg-surface/80 p-7 shadow-[var(--shadow-card)] backdrop-blur">
          <h1 className="text-2xl font-extrabold tracking-tight">{title}</h1>
          {subtitle && <p className="mt-1.5 text-sm text-muted">{subtitle}</p>}
          <div className="mt-6">{children}</div>
        </div>
      </main>
    </div>
  )
}

export function FormError({ error }: { error: Error | null }) {
  if (!error) return null
  return (
    <p role="alert" className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300">
      {error.message}
    </p>
  )
}
