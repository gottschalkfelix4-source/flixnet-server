import { Library, Play } from 'lucide-react'
import { useSystemInfo } from '@/api/queries/system'
import { EmptyState } from '@/components/ui/EmptyState'

export function HomePage() {
  const info = useSystemInfo()

  return (
    <div className="pb-16">
      <section className="relative isolate overflow-hidden">
        <div
          aria-hidden="true"
          className="absolute inset-0 -z-10 bg-[radial-gradient(900px_420px_at_75%_10%,rgb(255_107_61/0.22),transparent_60%),linear-gradient(135deg,#1d1d23,#0f0f12_60%)]"
        />
        <div
          aria-hidden="true"
          className="absolute inset-0 -z-10 bg-gradient-to-t from-canvas via-canvas/40 to-transparent"
        />
        <div className="flex min-h-[46vh] max-w-3xl flex-col justify-end gap-4 px-6 pt-24 pb-12 md:px-10">
          <p className="text-sm font-semibold tracking-widest text-accent uppercase">Willkommen</p>
          <h1 className="text-4xl font-extrabold tracking-tight md:text-6xl">Dein Kino. Dein Server.</h1>
          <p className="max-w-xl text-base text-muted md:text-lg">
            Filme, Serien, Musik und Live TV, organisiert von deinem eigenen Flixnet-Server, mit
            Empfehlungen, die lokal berechnet werden.
          </p>
          <div className="flex flex-wrap items-center gap-3 pt-2">
            <button
              type="button"
              disabled
              className="inline-flex h-11 items-center gap-2 rounded-full bg-fg px-6 font-bold text-canvas disabled:opacity-40"
            >
              <Play className="size-5 fill-current" aria-hidden="true" />
              Abspielen
            </button>
            <ServerBadge
              status={info.status}
              label={info.data ? `${info.data.name} ${info.data.version}` : undefined}
            />
          </div>
        </div>
      </section>

      <EmptyState icon={Library} title="Noch keine Bibliotheken">
        Sobald du Medienordner hinzufügst, erscheinen hier „Weiterschauen“, „Neu hinzugefügt“ und
        persönliche Empfehlungen.
      </EmptyState>
    </div>
  )
}

function ServerBadge({ status, label }: { status: 'pending' | 'error' | 'success'; label?: string }) {
  const text =
    status === 'success' ? label : status === 'error' ? 'Server nicht erreichbar' : 'Verbinde…'
  const dot = status === 'success' ? 'bg-emerald-400' : status === 'error' ? 'bg-red-500' : 'bg-subtle'
  return (
    <span
      data-testid="server-status"
      className="inline-flex h-11 items-center gap-2 rounded-full border border-line bg-surface/70 px-4 text-sm text-muted backdrop-blur"
    >
      <span className={`size-2 rounded-full ${dot}`} aria-hidden="true" />
      {text}
    </span>
  )
}
