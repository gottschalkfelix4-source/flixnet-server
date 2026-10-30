import * as Popover from '@radix-ui/react-dialog'
import clsx from 'clsx'
import { Check, Copy, LogOut, Users } from 'lucide-react'
import { useState } from 'react'
import { useCreateSyncPlayGroup, useSyncPlayGroup, useSyncPlayGroups } from '@/api/queries/syncplay'
import { copyText } from '@/lib/clipboard'
import { REACTIONS } from '@/lib/syncplay'
import type { SyncPlayHandle } from '@/lib/useSyncPlay'

interface SyncPlayPanelProps {
  itemId: number
  /** Group being followed (`null`: none). */
  groupId: string | null
  sync: SyncPlayHandle | null
  /** Current local position (ms), where a new group starts. */
  positionMs: number
  onJoin: (groupId: string) => void
  onLeave: () => void
}

/** "Gemeinsam schauen": start or join a group, invite others, members and reactions. */
export function SyncPlayPanel({ itemId, groupId, sync, positionMs, onJoin, onLeave }: SyncPlayPanelProps) {
  const [open, setOpen] = useState(false)
  const groups = useSyncPlayGroups(open && !groupId)
  const group = useSyncPlayGroup(groupId)
  const create = useCreateSyncPlayGroup()
  const [copied, setCopied] = useState(false)
  const members = sync?.state?.members ?? []
  const invite = group.data ? `${window.location.origin}/watch/${group.data.code}` : null
  const others = (groups.data ?? []).filter((g) => g.item_id === itemId)

  return (
    <Popover.Root open={open} onOpenChange={setOpen} modal={false}>
      <Popover.Trigger
        aria-label="Gemeinsam schauen"
        title="Gemeinsam schauen"
        className={clsx(
          'relative flex size-11 shrink-0 items-center justify-center rounded-full hover:bg-white/15 [&>svg]:pointer-events-none',
          groupId && 'text-[var(--color-accent)]',
        )}
        data-testid="syncplay-button"
      >
        <Users className="size-6" />
        {groupId && members.length > 0 && (
          <span className="absolute top-1 right-1 rounded-full bg-[var(--color-accent)] px-1 text-[10px] font-bold text-black">
            {members.length}
          </span>
        )}
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          className="fixed right-4 bottom-24 z-50 w-80 rounded-2xl border border-white/10 bg-black/90 p-4 text-white shadow-2xl backdrop-blur focus:outline-none"
          data-testid="syncplay-panel"
          aria-describedby={undefined}
        >
          <Popover.Title className="mb-3 text-base font-bold">Gemeinsam schauen</Popover.Title>
          {groupId ? (
            <div className="flex flex-col gap-3">
              <p className="text-sm text-white/70" data-testid="syncplay-status">
                {sync?.closed
                  ? (sync.error ?? 'Verbindung beendet.')
                  : sync?.connected
                    ? `${group.data?.name ?? 'Gruppe'} · ${members.length} ${members.length === 1 ? 'Person' : 'Personen'}`
                    : 'Verbinde …'}
              </p>
              <ul className="flex flex-col gap-1 text-sm" data-testid="syncplay-members">
                {members.map((m) => (
                  <li key={m.id} className="flex items-center gap-2">
                    <span className={clsx('size-2 rounded-full', m.ready ? 'bg-emerald-400' : 'bg-amber-400')} aria-hidden="true" />
                    {m.name}
                    {m.id === sync?.memberId && <span className="text-white/50">(du)</span>}
                  </li>
                ))}
              </ul>
              {sync?.state?.waiting && <p className="text-xs text-amber-300">Warte, bis alle bereit sind …</p>}
              {invite && (
                <div className="flex items-center gap-2">
                  <code className="min-w-0 flex-1 truncate rounded bg-white/10 px-2 py-1 text-xs" data-testid="syncplay-invite">
                    {invite}
                  </code>
                  <button
                    type="button"
                    aria-label="Einladungslink kopieren"
                    onClick={() => void copyText(invite).then((ok) => setCopied(ok))}
                    className="rounded-full p-2 hover:bg-white/15"
                  >
                    {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
                  </button>
                </div>
              )}
              <div className="flex flex-wrap gap-1" aria-label="Reaktionen">
                {REACTIONS.map((emoji) => (
                  <button
                    key={emoji}
                    type="button"
                    aria-label={`Reaktion ${emoji}`}
                    onClick={() => sync?.react(emoji)}
                    className="rounded-full px-1.5 py-1 text-lg hover:bg-white/15"
                  >
                    {emoji}
                  </button>
                ))}
              </div>
              <button
                type="button"
                onClick={onLeave}
                className="flex items-center justify-center gap-2 rounded-full bg-white/10 px-4 py-2 text-sm font-semibold hover:bg-white/20"
              >
                <LogOut className="size-4" aria-hidden="true" /> Gruppe verlassen
              </button>
            </div>
          ) : (
            <div className="flex flex-col gap-3">
              <p className="text-sm text-white/70">
                Schaut zusammen auf mehreren Geräten: Abspielen, Pause und Springen gelten für alle.
              </p>
              <button
                type="button"
                disabled={create.isPending}
                onClick={() =>
                  create.mutate(
                    { item_id: itemId, position_ms: Math.round(positionMs) },
                    { onSuccess: (g) => onJoin(g.id) },
                  )
                }
                className="rounded-full bg-[var(--color-accent)] px-4 py-2 text-sm font-bold text-black hover:opacity-90 disabled:opacity-50"
              >
                Gruppe starten
              </button>
              {create.error && <p className="text-xs text-red-300">{create.error.message}</p>}
              {others.length > 0 && (
                <ul className="flex flex-col gap-1">
                  {others.map((g) => (
                    <li key={g.id}>
                      <button
                        type="button"
                        onClick={() => onJoin(g.id)}
                        className="w-full rounded-lg px-3 py-2 text-left text-sm hover:bg-white/10"
                      >
                        <span className="font-semibold">{g.name}</span> beitreten
                        <span className="block text-xs text-white/60">{g.members.join(', ') || 'noch niemand verbunden'}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  )
}
