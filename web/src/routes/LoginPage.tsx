import { useMutation, useQuery } from '@tanstack/react-query'
import { useNavigate, useSearch } from '@tanstack/react-router'
import { KeyRound } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { api, unwrap } from '@/api/client'
import { useLogin } from '@/api/queries/auth'
import { oidcLoginConfigQuery } from '@/api/queries/ops'
import { deviceInfo } from '@/lib/session'
import { SSO_REDIRECT_KEY } from '@/lib/sso'
import { AuthLayout, FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { TextField } from '@/components/ui/TextField'

export function LoginPage() {
  const navigate = useNavigate()
  const { redirect } = useSearch({ from: '/login' })
  const login = useLogin()
  const [name, setName] = useState('')
  const [password, setPassword] = useState('')

  function submit(event: FormEvent) {
    event.preventDefault()
    login.mutate(
      { name: name.trim(), password },
      {
        // Only follow same-origin, path-only redirects.
        onSuccess: () =>
          navigate({
            to: redirect?.startsWith('/') && !redirect.startsWith('//') ? redirect : '/',
            replace: true,
          }),
      },
    )
  }

  return (
    <AuthLayout title="Anmelden" subtitle="Willkommen zurück bei Flixnet.">
      <form onSubmit={submit} className="flex flex-col gap-4">
        <TextField
          label="Name"
          autoComplete="username"
          autoFocus
          required
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <TextField
          label="Passwort"
          type="password"
          autoComplete="current-password"
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <FormError error={login.error} />
        <Button type="submit" variant="accent" loading={login.isPending} disabled={!name || !password}>
          Anmelden
        </Button>
      </form>
      <SsoButton redirect={redirect} />
    </AuthLayout>
  )
}

/** "Mit <Anbieter> anmelden" when single sign-on is set up. */
function SsoButton({ redirect }: { redirect?: string }) {
  const config = useQuery(oidcLoginConfigQuery)
  const start = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST('/api/v1/auth/oidc/start', {
          body: { redirect_uri: `${window.location.origin}/login/oidc`, device: deviceInfo() },
        }),
      ),
    onSuccess: ({ authorization_url }) => {
      // Where to go after the round trip through the provider.
      if (redirect?.startsWith('/') && !redirect.startsWith('//')) sessionStorage.setItem(SSO_REDIRECT_KEY, redirect)
      window.location.assign(authorization_url)
    },
  })
  if (!config.data?.enabled) return null
  return (
    <div className="mt-5 flex flex-col gap-3">
      <div className="flex items-center gap-3 text-xs text-subtle" aria-hidden="true">
        <span className="h-px flex-1 bg-line" />
        oder
        <span className="h-px flex-1 bg-line" />
      </div>
      <Button type="button" loading={start.isPending || start.isSuccess} onClick={() => start.mutate()}>
        <KeyRound className="size-4" aria-hidden="true" />
        Mit {config.data.provider_name} anmelden
      </Button>
      <FormError error={start.error} />
    </div>
  )
}
