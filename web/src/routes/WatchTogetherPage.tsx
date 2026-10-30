import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { LoaderCircle, Users } from 'lucide-react'
import { useEffect, useState } from 'react'
import { joinSyncPlayGroup } from '@/api/queries/syncplay'

/** `/watch/{code}`: an invitation to a SyncPlay group; joins it in the player. */
export function WatchTogetherPage() {
  const { code } = useParams({ from: '/watch/$code' })
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    joinSyncPlayGroup(code)
      .then((group) => {
        if (cancelled) return
        void navigate({
          to: '/play/$id',
          params: { id: String(group.item_id) },
          search: { group: group.id },
          replace: true,
        })
      })
      .catch(() => !cancelled && setError('Diese Einladung ist abgelaufen oder der Titel ist für dich nicht freigegeben.'))
    return () => {
      cancelled = true
    }
  }, [code, navigate])

  return (
    <div className="flex min-h-dvh flex-col items-center justify-center gap-4 p-6 text-center">
      <Users className="size-10 text-accent" aria-hidden="true" />
      {error ? (
        <>
          <p className="text-lg font-bold" role="alert">
            {error}
          </p>
          <Link to="/" className="rounded-full bg-surface-2 px-5 py-2 text-sm font-semibold hover:bg-surface-3">
            Zur Startseite
          </Link>
        </>
      ) : (
        <p className="flex items-center gap-2 text-muted" role="status">
          <LoaderCircle className="size-5 animate-spin" aria-hidden="true" /> Gemeinsam schauen wird geöffnet …
        </p>
      )}
    </div>
  )
}
