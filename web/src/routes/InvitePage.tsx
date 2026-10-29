import { useQuery } from '@tanstack/react-query'
import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { type FormEvent, useState } from 'react'
import { invitationStatusQuery, useRedeemInvitation } from '@/api/queries/users'
import { AuthLayout, FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { TextField } from '@/components/ui/TextField'

const MIN_PASSWORD = 8

export function InvitePage() {
  const { code } = useParams({ from: '/invite/$code' })
  const status = useQuery(invitationStatusQuery(code))
  const redeem = useRedeemInvitation(code)
  const navigate = useNavigate()
  const [name, setName] = useState('')
  const [password, setPassword] = useState('')
  const [localError, setLocalError] = useState<Error | null>(null)

  if (status.data && !status.data.valid) {
    return (
      <AuthLayout
        title="Einladung ungültig"
        subtitle="Dieser Link ist abgelaufen oder wurde bereits verwendet. Bitte frag nach einer neuen Einladung."
      >
        <Link to="/login" className="font-semibold text-accent hover:text-accent-hover">
          Zur Anmeldung
        </Link>
      </AuthLayout>
    )
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    if (password.length < MIN_PASSWORD) {
      setLocalError(new Error(`Das Passwort muss mindestens ${MIN_PASSWORD} Zeichen lang sein.`))
      return
    }
    setLocalError(null)
    redeem.mutate(
      { name: name.trim(), password },
      { onSuccess: () => navigate({ to: '/', replace: true }) },
    )
  }

  return (
    <AuthLayout title="Du bist eingeladen" subtitle="Lege dein Flixnet-Konto an.">
      <form onSubmit={submit} className="flex flex-col gap-4" noValidate>
        <TextField
          label="Name"
          autoComplete="username"
          autoFocus
          maxLength={64}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <TextField
          label="Passwort"
          type="password"
          autoComplete="new-password"
          hint={`Mindestens ${MIN_PASSWORD} Zeichen.`}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <FormError error={localError ?? redeem.error} />
        <Button
          type="submit"
          variant="accent"
          loading={redeem.isPending}
          disabled={!name.trim() || status.isPending}
        >
          Konto anlegen
        </Button>
      </form>
    </AuthLayout>
  )
}
