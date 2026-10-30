import { useNavigate } from '@tanstack/react-router'
import { Ban, Lock, LockKeyhole } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import type { Schemas } from '@/api/client'
import { useProfiles, useSwitchProfile } from '@/api/queries/users'
import { FormError } from '@/components/layout/AuthLayout'
import { Logo } from '@/components/layout/Logo'
import { Avatar } from '@/components/ui/Avatar'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { TextField } from '@/components/ui/TextField'

type Profile = Schemas['ProfileDto']

/** "Who's watching?" — switch this device between household profiles. */
export function ProfilesPage() {
  const profiles = useProfiles()
  const switchProfile = useSwitchProfile()
  const navigate = useNavigate()
  const [locked, setLocked] = useState<Profile | null>(null)
  const [blocked, setBlocked] = useState<Profile | null>(null)

  function choose(profile: Profile) {
    setBlocked(null)
    if (profile.is_current) {
      void navigate({ to: '/' })
    } else if (profile.unlock === 'locked') {
      setBlocked(profile)
    } else if (profile.unlock === 'none') {
      switchProfile.mutate({ user_id: profile.id }, { onSuccess: () => navigate({ to: '/' }) })
    } else {
      switchProfile.reset()
      setLocked(profile)
    }
  }

  return (
    <div className="flex min-h-dvh flex-col items-center justify-center px-6 py-12">
      <div className="mb-10">
        <Logo />
      </div>
      <h1 className="mb-10 text-3xl font-extrabold tracking-tight md:text-5xl">Wer schaut?</h1>

      <ul className="flex max-w-4xl flex-wrap justify-center gap-6 md:gap-10">
        {profiles.data?.map((profile) => (
          <li key={profile.id}>
            <button
              type="button"
              onClick={() => choose(profile)}
              className="group flex w-32 flex-col items-center gap-3 rounded-2xl p-2 focus-visible:outline-accent"
            >
              <span className="relative">
                <Avatar
                  id={profile.id}
                  name={profile.name}
                  size="xl"
                  className="ring-0 ring-fg transition group-hover:ring-4 group-focus-visible:ring-4"
                />
                {profile.unlock !== 'none' && (
                  <span className="absolute -right-2 -bottom-2 rounded-full border border-line bg-surface p-1.5 text-muted">
                    {profile.unlock === 'pin' ? (
                      <Lock className="size-4" aria-label="PIN-geschützt" />
                    ) : profile.unlock === 'locked' ? (
                      <Ban className="size-4" aria-label="Gesperrt" />
                    ) : (
                      <LockKeyhole className="size-4" aria-label="Passwort-geschützt" />
                    )}
                  </span>
                )}
              </span>
              <span className="text-base font-semibold text-muted group-hover:text-fg">
                {profile.name}
              </span>
              {profile.is_current && <span className="sr-only">(aktuelles Profil)</span>}
            </button>
          </li>
        ))}
      </ul>
      <FormError error={profiles.error ?? (locked ? null : switchProfile.error)} />
      {blocked && (
        <p role="alert" className="mt-6 max-w-md text-center text-sm text-muted">
          Zu „{blocked.name}“ kann von diesem Profil aus nicht gewechselt werden: Es hat weniger
          Jugendschutz-Beschränkungen, aber keine PIN. Ein Administrator kann eine PIN festlegen.
        </p>
      )}

      <UnlockDialog
        profile={locked}
        onClose={() => setLocked(null)}
        pending={switchProfile.isPending}
        error={switchProfile.error}
        onSubmit={(secret) =>
          locked &&
          switchProfile.mutate(
            locked.unlock === 'pin'
              ? { user_id: locked.id, pin: secret }
              : { user_id: locked.id, password: secret },
            { onSuccess: () => navigate({ to: '/' }) },
          )
        }
      />
    </div>
  )
}

function UnlockDialog({
  profile,
  onClose,
  onSubmit,
  pending,
  error,
}: {
  profile: Profile | null
  onClose: () => void
  onSubmit: (secret: string) => void
  pending: boolean
  error: Error | null
}) {
  const [secret, setSecret] = useState('')
  const isPin = profile?.unlock === 'pin'

  function submit(event: FormEvent) {
    event.preventDefault()
    onSubmit(secret)
  }

  return (
    <Dialog
      open={profile !== null}
      onOpenChange={(open) => {
        if (!open) {
          setSecret('')
          onClose()
        }
      }}
      title={profile ? `Profil „${profile.name}“` : ''}
      description={isPin ? 'Gib die PIN dieses Profils ein.' : 'Gib das Passwort dieses Profils ein.'}
    >
      <form onSubmit={submit} className="flex flex-col gap-4">
        <TextField
          label={isPin ? 'PIN' : 'Passwort'}
          type="password"
          inputMode={isPin ? 'numeric' : undefined}
          autoComplete={isPin ? 'off' : 'current-password'}
          autoFocus
          value={secret}
          onChange={(e) => setSecret(isPin ? e.target.value.replace(/\D/g, '') : e.target.value)}
          maxLength={isPin ? 8 : undefined}
        />
        <FormError error={error} />
        <Button type="submit" variant="accent" loading={pending} disabled={!secret}>
          Entsperren
        </Button>
      </form>
    </Dialog>
  )
}
