import clsx from 'clsx'
import { CheckCircle2, Cpu, LoaderCircle, XCircle } from 'lucide-react'
import type { ReactNode } from 'react'
import type { Schemas } from '@/api/client'
import { useTranscodingInfo } from '@/api/queries/system'
import { Section } from '@/components/ui/Section'

type Encoder = Schemas['EncoderKind']
type Accel = Schemas['Accel']
type Tonemap = Schemas['Tonemap']

const ENCODER_LABELS: Record<Encoder, string> = {
  software: 'Software (libx264, CPU)',
  nvenc: 'NVIDIA NVENC',
  qsv: 'Intel Quick Sync',
  vaapi: 'VA-API',
}

const ACCEL_LABELS: Record<Accel, string> = {
  nvenc: 'NVIDIA NVENC',
  qsv: 'Intel Quick Sync',
  vaapi: 'VA-API',
}

const MODE_LABELS: Record<Schemas['HardwareMode'], string> = {
  auto: 'Automatisch (beste verfügbare)',
  none: 'Aus (nur Software)',
  qsv: 'Intel Quick Sync',
  vaapi: 'VA-API',
  nvenc: 'NVIDIA NVENC',
}

const TONEMAP_LABELS: Record<Tonemap, string> = {
  software: 'Software (CPU, zscale)',
  cuda: 'GPU (tonemap_cuda)',
  vpp_qsv: 'GPU (Quick Sync VPP)',
  vaapi: 'GPU (tonemap_vaapi)',
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap gap-x-6 gap-y-1 py-2.5">
      <dt className="w-48 shrink-0 text-sm text-muted">{label}</dt>
      <dd className="min-w-0 flex-1 text-sm font-semibold">{children}</dd>
    </div>
  )
}

function Probe({ probe }: { probe: Schemas['EncoderProbe'] }) {
  return (
    <li className="py-3">
      <p className="flex items-center gap-2 text-sm font-semibold">
        {probe.available ? (
          <CheckCircle2 className="size-4 text-emerald-400" aria-hidden="true" />
        ) : (
          <XCircle className="size-4 text-red-400" aria-hidden="true" />
        )}
        {ACCEL_LABELS[probe.accel]}
        <span
          className={clsx(
            'rounded-full px-2 py-0.5 text-xs font-bold',
            probe.available ? 'bg-emerald-500/15 text-emerald-400' : 'bg-surface-3 text-muted',
          )}
        >
          {probe.available ? 'verfügbar' : 'nicht verfügbar'}
        </span>
      </p>
      <p className="mt-1 text-xs text-subtle">
        {probe.available
          ? [
              probe.hw_decode ? 'Dekodierung auf der GPU' : 'Dekodierung auf der CPU',
              `Tone-Mapping: ${TONEMAP_LABELS[probe.tonemap]}`,
              probe.low_power ? 'Low-Power-Encoder' : null,
            ]
              .filter(Boolean)
              .join(' · ')
          : 'Test-Encode fehlgeschlagen'}
        {probe.device ? ` · ${probe.device}` : ''} · Test {(probe.duration_ms / 1000).toFixed(1)} s
      </p>
      {probe.error && <p className="mt-1 break-words font-mono text-xs text-red-400/90">{probe.error}</p>}
      {[
        { label: 'GPU-Dekodierung nicht möglich', text: probe.decode_error },
        { label: 'GPU-Tone-Mapping nicht möglich', text: probe.tonemap_error },
      ]
        .filter((note) => !!note.text)
        .map((note) => (
          <p key={note.label} className="mt-1 break-words text-xs text-amber-400/90">
            {note.label}: <span className="font-mono">{note.text}</span>
          </p>
        ))}
    </li>
  )
}

export function TranscodingSettings() {
  const info = useTranscodingInfo()
  const data = info.data

  return (
    <div className="flex flex-col gap-6">
      <Section
        title="Hardware-Transcoding"
        description="Beim Start testet Flixnet, welche Grafik-Encoder funktionieren. Schlägt eine Hardware-Umwandlung fehl, versucht Flixnet es automatisch mit CPU-Dekodierung und zuletzt ganz in Software."
      >
        {!data ? (
          <p className="text-sm text-muted">Wird geladen …</p>
        ) : (
          <dl className="divide-y divide-line/60" data-testid="transcoding-info">
            <Row label="Einstellung">
              {MODE_LABELS[data.mode]}
              <span className="ml-2 font-mono text-xs font-normal text-subtle">media.hardware_acceleration</span>
            </Row>
            <Row label="Aktiver Encoder">
              {data.detected ? (
                <span className="inline-flex items-center gap-2">
                  <Cpu className="size-4 text-accent" aria-hidden="true" />
                  {ENCODER_LABELS[data.encoder]}
                </span>
              ) : (
                <span className="inline-flex items-center gap-2 text-muted">
                  <LoaderCircle className="size-4 animate-spin" aria-hidden="true" />
                  Hardware wird getestet …
                </span>
              )}
            </Row>
            {data.encoder !== 'software' && (
              <Row label="Dekodierung">{data.hw_decode ? 'GPU' : 'CPU'}</Row>
            )}
            <Row label="HDR → SDR">{TONEMAP_LABELS[data.tonemap]}</Row>
            <Row label="Rückfälle seit Start">{data.fallbacks}</Row>
            <Row label="ffmpeg">
              <span className="break-words font-mono text-xs font-normal">
                {data.ffmpeg_version?.split(' Copyright')[0] ?? 'unbekannt'}
              </span>
            </Row>
          </dl>
        )}
      </Section>

      {data?.detected && (
        <Section title="Getestete Verfahren" description="Ergebnis der Tests beim Serverstart.">
          {data.probes.length === 0 ? (
            <p className="text-sm text-muted">
              {data.mode === 'none'
                ? 'Hardware-Transcoding ist ausgeschaltet.'
                : 'Auf diesem System ist kein Hardware-Verfahren möglich.'}
            </p>
          ) : (
            <ul className="divide-y divide-line/60">
              {data.probes.map((probe) => (
                <Probe key={probe.accel} probe={probe} />
              ))}
            </ul>
          )}
        </Section>
      )}
    </div>
  )
}
