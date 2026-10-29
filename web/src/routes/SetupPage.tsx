import { useNavigate } from '@tanstack/react-router'
import { type FormEvent, useState } from 'react'
import { useCompleteSetup } from '@/api/queries/auth'
import { AuthLayout, FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { TextField } from '@/components/ui/TextField'

const MIN_PASSWORD = 8

export function SetupPage() {
  const navigate = useNavigate()
  const setup = useCompleteSetup()
  const [name, setName] = useState('')
  const [password, setPassword] = useState('')
  const [repeat, setRepeat] = useState('')
  const [localError, setLocalError] = useState<Error | null>(null)

  function submit(event: FormEvent) {
    event.preventDefault()
    if (password.length < MIN_PASSWORD) {
      setLocalError(new Error(`Das Passwort muss mindestens ${MIN_PASSWORD} Zeichen lang sein.`))
      return
    }
    if (password !== repeat) {
      setLocalError(new Error('Die Passwörter stimmen nicht überein.'))
      return
    }
    setLocalError(null)
    setup.mutate(
      { name: name.trim(), password },
      { onSuccess: () => navigate({ to: '/', replace: true }) },
    )
  }

  return (
    <AuthLayout
      title="Flixnet einrichten"
      subtitle="Lege das Administrator-Konto an. Damit verwaltest du später Bibliotheken und Nutzer."
    >
      <form onSubmit={submit} className="flex flex-col gap-4" noValidate>
        <TextField
          label="Name"
          autoComplete="username"
          autoFocus
          required
          maxLength={64}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <TextField
          label="Passwort"
          type="password"
          autoComplete="new-password"
          required
          hint={`Mindestens ${MIN_PASSWORD} Zeichen.`}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <TextField
          label="Passwort wiederholen"
          type="password"
          autoComplete="new-password"
          required
          value={repeat}
          onChange={(e) => setRepeat(e.target.value)}
        />
        <FormError error={localError ?? setup.error} />
        <Button type="submit" variant="accent" loading={setup.isPending} disabled={!name.trim()}>
          Konto anlegen
        </Button>
      </form>
    </AuthLayout>
  )
}
