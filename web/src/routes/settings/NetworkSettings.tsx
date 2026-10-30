import { CheckCircle2, Radio, TriangleAlert } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import type { Schemas } from '@/api/client'
import { useNetworkSettings, useUpdateNetworkSettings } from '@/api/queries/network'
import { useUsers } from '@/api/queries/users'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

type NetworkSettingsDto = Schemas['NetworkSettingsDto']

/** Public/LAN address (Chromecast) and the DLNA server. */
export function NetworkSettings() {
  const settings = useNetworkSettings()
  if (!settings.data) return null
  return (
    <div className="flex flex-col gap-6">
      <AddressSection settings={settings.data} />
      <DlnaSection settings={settings.data} />
    </div>
  )
}

function AddressSection({ settings }: { settings: NetworkSettingsDto }) {
  const update = useUpdateNetworkSettings()
  const [url, setUrl] = useState(settings.public_url ?? '')
  const [saved, setSaved] = useState(false)
  return (
    <Section
      title="Öffentliche/LAN-Adresse"
      description="Unter dieser Adresse erreichen Geräte im Netzwerk den Server, z. B. ein Chromecast. Leer lassen, um die Adresse des Browsers zu verwenden."
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(e: FormEvent) => {
          e.preventDefault()
          setSaved(false)
          update.mutate({ public_url: url.trim() || null }, { onSuccess: () => setSaved(true) })
        }}
      >
        <TextField
          label="Adresse"
          placeholder="http://192.168.1.10:8097"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          hint="Mit http:// oder https:// und Port; nicht localhost (das erreicht ein Chromecast nicht)."
        />
        <FormError error={update.error} />
        <div className="flex items-center gap-3">
          <Button type="submit" variant="accent" loading={update.isPending}>
            Speichern
          </Button>
          {saved && (
            <span className="flex items-center gap-1.5 text-sm text-muted" role="status">
              <CheckCircle2 className="size-4 text-emerald-400" aria-hidden="true" /> Gespeichert
            </span>
          )}
        </div>
      </form>
    </Section>
  )
}

function DlnaSection({ settings }: { settings: NetworkSettingsDto }) {
  const users = useUsers()
  const update = useUpdateNetworkSettings()
  const [userId, setUserId] = useState(settings.dlna_user_id ? String(settings.dlna_user_id) : '')
  const [name, setName] = useState(settings.dlna_name)
  const [port, setPort] = useState(String(settings.dlna_port))
  const status = settings.dlna_status

  return (
    <Section
      title="DLNA"
      description="Fernseher, Receiver und Apps wie VLC finden Flixnet im Netzwerk. DLNA-Geräte melden sich nicht an: Sie sehen genau das, was der gewählte Benutzer sehen darf (Bibliotheken und Jugendschutz)."
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(e: FormEvent) => {
          e.preventDefault()
          update.mutate({
            dlna_user_id: userId ? Number(userId) : null,
            dlna_name: name,
            dlna_port: Number(port) || 8200,
          })
        }}
      >
        <SelectField
          label="Benutzer für DLNA"
          value={userId}
          onChange={setUserId}
          options={[
            { value: '', label: 'Aus' },
            ...(users.data ?? []).map((u) => ({ value: String(u.id), label: u.name })),
          ]}
        />
        <div className="grid gap-4 sm:grid-cols-[1fr_10rem]">
          <TextField label="Name im Netzwerk" value={name} maxLength={60} onChange={(e) => setName(e.target.value)} />
          <TextField label="Port" inputMode="numeric" value={port} onChange={(e) => setPort(e.target.value)} />
        </div>
        <FormError error={update.error} />
        <div className="flex flex-wrap items-center gap-3">
          <Button type="submit" variant="accent" loading={update.isPending}>
            Speichern
          </Button>
          <p className="flex items-center gap-1.5 text-sm text-muted" data-testid="dlna-status">
            {status.running ? (
              <>
                <Radio className="size-4 text-emerald-400" aria-hidden="true" />
                Läuft auf Port {status.port}
                {status.discovery ? ' · im Netzwerk sichtbar' : ''}
              </>
            ) : (
              'Aus'
            )}
          </p>
        </div>
        {status.error && (
          <p className="flex items-start gap-2 text-sm text-amber-300" role="alert">
            <TriangleAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
            {status.error}
          </p>
        )}
      </form>
    </Section>
  )
}
