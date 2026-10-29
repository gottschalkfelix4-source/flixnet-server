import { useNavigate, useSearch } from '@tanstack/react-router'
import { type FormEvent, useState } from 'react'
import { useLogin } from '@/api/queries/auth'
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
    </AuthLayout>
  )
}
