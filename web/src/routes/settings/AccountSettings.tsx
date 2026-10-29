import { Monitor, Smartphone, Tv } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { currentUserQuery } from '@/api/queries/auth'
import { useChangePassword, useMyDevices, useSetPin, useSignOutDevice } from '@/api/queries/users'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Section } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { relativeTime } from '@/lib/format'

export function AccountSettings() {
  return (
    <div className="flex flex-col gap-6">
      <PasswordForm />
      <PinForm />
      <DevicesList />
    </div>
  )
}

function PasswordForm() {
  const change = useChangePassword()
  const me = useQuery(currentUserQuery)
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [done, setDone] = useState(false)

  function submit(event: FormEvent) {
    event.preventDefault()
    setDone(false)
    change.mutate(
      { current_password: current, new_password: next },
      {
        onSuccess: () => {
          setCurrent('')
          setNext('')
          setDone(true)
        },
      },
    )
  }

  return (
    <Section title="Passwort" description="Nach der Änderung werden alle anderen Geräte abgemeldet.">
      <form onSubmit={submit} className="grid max-w-md gap-4">
        {me.data?.has_password !== false && (
          <TextField
            label="Aktuelles Passwort"
            type="password"
            autoComplete="current-password"
            value={current}
            onChange={(e) => setCurrent(e.target.value)}
          />
        )}
        <TextField
          label="Neues Passwort"
          type="password"
          autoComplete="new-password"
          hint="Mindestens 8 Zeichen."
          value={next}
          onChange={(e) => setNext(e.target.value)}
        />
        <FormError error={change.error} />
        {done && (
          <p role="status" className="text-sm text-emerald-400">
            Passwort geändert.
          </p>
        )}
        <div>
          <Button type="submit" loading={change.isPending} disabled={!next}>
            Passwort ändern
          </Button>
        </div>
      </form>
    </Section>
  )
}

function PinForm() {
  const setPin = useSetPin()
  const me = useQuery(currentUserQuery)
  const [pin, setPinValue] = useState('')

  function submit(event: FormEvent) {
    event.preventDefault()
    setPin.mutate(pin, { onSuccess: () => setPinValue('') })
  }

  return (
    <Section
      title="Profil-PIN"
      description="Schützt den Wechsel zu deinem Profil auf gemeinsam genutzten Geräten (z. B. dem Wohnzimmer-TV)."
    >
      <form onSubmit={submit} className="flex max-w-md flex-wrap items-end gap-3">
        <TextField
          className="w-40"
          label={me.data?.has_pin ? 'Neue PIN' : 'PIN'}
          type="password"
          inputMode="numeric"
          autoComplete="off"
          maxLength={8}
          hint="4–8 Ziffern"
          value={pin}
          onChange={(e) => setPinValue(e.target.value.replace(/\D/g, ''))}
        />
        <Button type="submit" loading={setPin.isPending && pin !== ''} disabled={pin.length < 4}>
          {me.data?.has_pin ? 'PIN ändern' : 'PIN festlegen'}
        </Button>
        {me.data?.has_pin && (
          <Button
            type="button"
            variant="ghost"
            onClick={() => setPin.mutate(null)}
            loading={setPin.isPending && pin === ''}
          >
            PIN entfernen
          </Button>
        )}
      </form>
      <div className="mt-3">
        <FormError error={setPin.error} />
      </div>
    </Section>
  )
}

function deviceIcon(name: string, client: string) {
  const text = `${name} ${client}`.toLowerCase()
  if (/tv|fire|roku|shield/.test(text)) return Tv
  if (/android|ios|iphone|ipad/.test(text)) return Smartphone
  return Monitor
}

function DevicesList() {
  const devices = useMyDevices()
  const signOut = useSignOutDevice()

  return (
    <Section title="Angemeldete Geräte">
      <ul className="divide-y divide-line/60">
        {devices.data?.map((device) => {
          const Icon = deviceIcon(device.name, device.client)
          return (
            <li key={device.id} className="flex items-center gap-4 py-3">
              <Icon className="size-5 shrink-0 text-muted" aria-hidden="true" />
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-semibold">
                  {device.name}
                  {device.is_current && (
                    <span className="ml-2 rounded-full bg-accent/15 px-2 py-0.5 text-xs font-bold text-accent">
                      Dieses Gerät
                    </span>
                  )}
                </p>
                <p className="text-xs text-subtle">
                  {device.client} · zuletzt aktiv {relativeTime(device.last_seen)}
                </p>
              </div>
              {!device.is_current && (
                <Button
                  variant="ghost"
                  className="h-9 px-4"
                  onClick={() => signOut.mutate(device.id)}
                  loading={signOut.isPending && signOut.variables === device.id}
                >
                  Abmelden
                </Button>
              )}
            </li>
          )
        })}
      </ul>
    </Section>
  )
}
