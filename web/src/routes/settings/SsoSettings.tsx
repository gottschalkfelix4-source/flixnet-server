import { type FormEvent, useState } from 'react'
import { useOidcSettings, useUpdateOidcSettings } from '@/api/queries/ops'
import type { Schemas } from '@/api/client'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Checkbox, Section } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

type Settings = Schemas['OidcSettingsDto']

function Form({ settings }: { settings: Settings }) {
  const update = useUpdateOidcSettings()
  const [draft, setDraft] = useState({ ...settings, client_secret: '', admin_group: settings.admin_group ?? '' })
  const [saved, setSaved] = useState(false)
  const set = <K extends keyof typeof draft>(key: K, value: (typeof draft)[K]) => {
    setSaved(false)
    setDraft((d) => ({ ...d, [key]: value }))
  }
  const callback = `${window.location.origin}/login/oidc`

  function submit(event: FormEvent) {
    event.preventDefault()
    update.mutate(
      {
        enabled: draft.enabled,
        provider_name: draft.provider_name,
        issuer_url: draft.issuer_url,
        client_id: draft.client_id,
        client_secret: draft.client_secret || undefined,
        scopes: draft.scopes,
        auto_create_users: draft.auto_create_users,
        link_by_name: draft.link_by_name,
        admin_group: draft.admin_group,
        groups_claim: draft.groups_claim,
      },
      {
        onSuccess: (data) => {
          setDraft({ ...data, client_secret: '', admin_group: data.admin_group ?? '' })
          setSaved(true)
        },
      },
    )
  }

  return (
    <form onSubmit={submit} className="flex flex-col gap-5">
      <Checkbox
        label="Anmeldung über OpenID Connect anbieten"
        hint="Zeigt auf der Anmeldeseite „Mit … anmelden“. Die Anmeldung mit Name und Passwort bleibt möglich."
        checked={draft.enabled}
        onChange={(v) => set('enabled', v)}
      />
      <div className="grid gap-4 md:grid-cols-2">
        <TextField label="Anzeigename" placeholder="Authentik" value={draft.provider_name} onChange={(e) => set('provider_name', e.target.value)} />
        <TextField
          label="Issuer-Adresse"
          type="url"
          placeholder="https://auth.example.org/application/o/flixnet/"
          value={draft.issuer_url}
          onChange={(e) => set('issuer_url', e.target.value)}
        />
        <TextField label="Client-ID" autoComplete="off" value={draft.client_id} onChange={(e) => set('client_id', e.target.value)} />
        <TextField
          label={settings.has_client_secret ? 'Neues Client-Secret (gespeichert)' : 'Client-Secret'}
          type="password"
          autoComplete="off"
          value={draft.client_secret}
          onChange={(e) => set('client_secret', e.target.value)}
        />
        <TextField label="Scopes" value={draft.scopes} onChange={(e) => set('scopes', e.target.value)} />
        <TextField
          label="Admin-Gruppe (optional)"
          hint="Mitglieder werden Administratoren."
          value={draft.admin_group}
          onChange={(e) => set('admin_group', e.target.value)}
        />
        <TextField label="Gruppen-Claim" value={draft.groups_claim} onChange={(e) => set('groups_claim', e.target.value)} />
      </div>
      <Checkbox
        label="Unbekannte Konten automatisch anlegen"
        hint="Sonst bekommt nur Zugang, wer schon mit einem Flixnet-Konto verknüpft ist."
        checked={draft.auto_create_users}
        onChange={(v) => set('auto_create_users', v)}
      />
      <Checkbox
        label="Bestehende Konten über den Namen verknüpfen"
        hint="preferred_username = Flixnet-Name. Nur einschalten, wenn der Anbieter diese Namen kontrolliert."
        checked={draft.link_by_name}
        onChange={(v) => set('link_by_name', v)}
      />
      <p className="rounded-lg bg-surface-2 p-3 text-sm text-muted">
        Beim Anbieter als Weiterleitungs-URL eintragen: <code className="text-fg">{callback}</code>
      </p>
      <FormError error={update.error} />
      {saved && (
        <p role="status" className="text-sm text-emerald-400">
          Gespeichert.
        </p>
      )}
      <Button type="submit" variant="accent" className="self-start" loading={update.isPending}>
        Speichern
      </Button>
    </form>
  )
}

/** OpenID Connect single sign-on (Authelia, Authentik, Keycloak …). */
export function SsoSettings() {
  const settings = useOidcSettings()
  return (
    <Section
      title="Single Sign-on"
      description="Anmeldung über einen eigenen Identitätsanbieter wie Authelia, Authentik oder Keycloak (OpenID Connect mit PKCE)."
    >
      {settings.data && <Form settings={settings.data} />}
    </Section>
  )
}
