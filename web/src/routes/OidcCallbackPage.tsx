import { useMutation } from '@tanstack/react-query'
import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import { LoaderCircle } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { api, unwrap } from '@/api/client'
import { AuthLayout, FormError } from '@/components/layout/AuthLayout'
import { setSession } from '@/lib/session'
import { SSO_REDIRECT_KEY } from '@/lib/sso'

export interface OidcCallbackSearch {
  code?: string
  state?: string
  error?: string
  error_description?: string
}

/** Where the identity provider sends the browser back: finishes the sign-in. */
export function OidcCallbackPage() {
  const search = useSearch({ from: '/login/oidc' })
  const navigate = useNavigate()
  const complete = useMutation({
    mutationFn: async ({ code, state }: { code: string; state: string }) =>
      unwrap(await api.POST('/api/v1/auth/oidc/complete', { body: { code, state } })),
    onSuccess: (session) => {
      setSession(session)
      const target = sessionStorage.getItem(SSO_REDIRECT_KEY)
      sessionStorage.removeItem(SSO_REDIRECT_KEY)
      void navigate({ to: target?.startsWith('/') && !target.startsWith('//') ? target : '/', replace: true })
    },
  })
  const { mutate } = complete
  // The state is single-use: never send it twice (StrictMode runs effects twice).
  const sent = useRef(false)

  useEffect(() => {
    if (sent.current || !search.code || !search.state) return
    sent.current = true
    mutate({ code: search.code, state: search.state })
  }, [search.code, search.state, mutate])

  const providerError = search.error ? (search.error_description ?? search.error) : null
  return (
    <AuthLayout title="Anmeldung" subtitle="Single Sign-on">
      {providerError || complete.isError || (!search.code && !providerError) ? (
        <div className="flex flex-col gap-4">
          {providerError && <FormError error={new Error(`Der Anmeldedienst meldet: ${providerError}`)} />}
          {!providerError && !search.code && <FormError error={new Error('Die Rückmeldung des Anmeldedienstes ist unvollständig.')} />}
          <FormError error={complete.error} />
          <Link to="/login" className="text-sm font-semibold text-accent hover:text-accent-hover">
            Zurück zur Anmeldung
          </Link>
        </div>
      ) : (
        <p className="flex items-center gap-2 text-sm text-muted" role="status">
          <LoaderCircle className="size-4 animate-spin" aria-hidden="true" />
          Anmeldung wird abgeschlossen …
        </p>
      )}
    </AuthLayout>
  )
}
