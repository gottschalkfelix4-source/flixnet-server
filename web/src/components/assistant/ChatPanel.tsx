import * as RadixDialog from '@radix-ui/react-dialog'
import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { MessageCircle, Play, Send, Sparkles, Trash2, X } from 'lucide-react'
import { type FormEvent, useEffect, useRef, useState } from 'react'
import { type ChatTurn, useAssistantStatus, useChat } from '@/api/queries/assistant'
import { BlurImage } from '@/components/media/BlurImage'

const SUGGESTIONS = [
  'Was soll ich heute Abend schauen?',
  'Wo war ich zuletzt stehen geblieben?',
  'Etwas Lustiges unter 90 Minuten',
]

/** "Frag Flixnet": the assistant's chat in a side panel, opened from the sidebar
 *  (only with an LLM configured). */
export function ChatPanel({ collapsed }: { collapsed: boolean }) {
  const status = useAssistantStatus()
  const [open, setOpen] = useState(false)
  if (!status.data?.llm_enabled) return null
  return (
    <RadixDialog.Root open={open} onOpenChange={setOpen}>
      <RadixDialog.Trigger asChild>
        <button
          type="button"
          title={collapsed ? 'Frag Flixnet' : undefined}
          className={clsx(
            'flex h-10 w-full items-center gap-3 rounded-lg text-sm font-semibold text-accent transition-colors hover:bg-surface-2',
            collapsed ? 'justify-center' : 'px-3',
          )}
        >
          <MessageCircle className="size-5 shrink-0" aria-hidden="true" />
          <span className={clsx(collapsed && 'sr-only')}>Frag Flixnet</span>
        </button>
      </RadixDialog.Trigger>
      <RadixDialog.Portal>
        <RadixDialog.Overlay className="fixed inset-0 z-40 bg-black/40" />
        <RadixDialog.Content className="fixed top-0 right-0 z-50 flex h-dvh w-full max-w-md flex-col border-l border-line bg-surface shadow-2xl focus:outline-none">
          <Conversation onNavigate={() => setOpen(false)} />
        </RadixDialog.Content>
      </RadixDialog.Portal>
    </RadixDialog.Root>
  )
}

function Conversation({ onNavigate }: { onNavigate: () => void }) {
  const chat = useChat()
  const [turns, setTurns] = useState<ChatTurn[]>([])
  const [input, setInput] = useState('')
  const end = useRef<HTMLDivElement>(null)

  useEffect(() => {
    end.current?.scrollIntoView({ block: 'end' })
  }, [turns, chat.isPending])

  function ask(text: string) {
    const question = text.trim()
    if (!question || chat.isPending) return
    const next: ChatTurn[] = [...turns, { role: 'user', content: question }]
    setTurns(next)
    setInput('')
    chat.mutate(next, {
      onSuccess: (r) =>
        setTurns((t) => [...t, { role: 'assistant', content: r.reply, actions: r.actions, items: r.items }]),
    })
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    ask(input)
  }

  return (
    <>
      <header className="flex items-center justify-between gap-3 border-b border-line px-5 py-4">
        <div>
          <RadixDialog.Title className="flex items-center gap-2 text-lg font-extrabold">
            <Sparkles className="size-5 text-accent" aria-hidden="true" />
            Frag Flixnet
          </RadixDialog.Title>
          <RadixDialog.Description className="text-xs text-muted">
            Sucht in deiner Bibliothek, deinem Verlauf und in Dialogen.
          </RadixDialog.Description>
        </div>
        <div className="flex gap-1">
          {turns.length > 0 && (
            <button
              type="button"
              aria-label="Unterhaltung löschen"
              onClick={() => setTurns([])}
              className="rounded-lg p-1.5 text-muted hover:bg-surface-2 hover:text-fg"
            >
              <Trash2 className="size-5" />
            </button>
          )}
          <RadixDialog.Close aria-label="Schließen" className="rounded-lg p-1.5 text-muted hover:bg-surface-2 hover:text-fg">
            <X className="size-5" />
          </RadixDialog.Close>
        </div>
      </header>

      <div className="flex-1 overflow-y-auto px-5 py-4" data-testid="chat-log" aria-live="polite">
        {turns.length === 0 && (
          <div className="flex flex-col gap-2">
            <p className="mb-2 text-sm text-muted">Zum Beispiel:</p>
            {SUGGESTIONS.map((s) => (
              <button
                key={s}
                type="button"
                onClick={() => ask(s)}
                className="rounded-xl border border-line px-4 py-2.5 text-left text-sm hover:border-accent hover:bg-surface-2"
              >
                {s}
              </button>
            ))}
          </div>
        )}
        <div className="flex flex-col gap-4">
          {turns.map((turn, i) => (
            <Turn key={i} turn={turn} onNavigate={onNavigate} />
          ))}
          {chat.isPending && (
            <p className="text-sm text-muted" role="status">
              Flixnet denkt nach…
            </p>
          )}
          {chat.error && (
            <p role="alert" className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300">
              {chat.error.message}
            </p>
          )}
        </div>
        <div ref={end} />
      </div>

      <form onSubmit={submit} className="flex gap-2 border-t border-line p-4">
        <input
          aria-label="Frage an Flixnet"
          placeholder="Frag etwas…"
          value={input}
          maxLength={2000}
          onChange={(e) => setInput(e.target.value)}
          className="h-11 min-w-0 flex-1 rounded-full border border-line bg-surface-2 px-4 focus:border-accent focus:outline-none"
        />
        <button
          type="submit"
          aria-label="Senden"
          disabled={!input.trim() || chat.isPending}
          className="inline-flex size-11 items-center justify-center rounded-full bg-accent text-accent-fg hover:bg-accent-hover disabled:opacity-50"
        >
          <Send className="size-5" />
        </button>
      </form>
    </>
  )
}

function Turn({ turn, onNavigate }: { turn: ChatTurn; onNavigate: () => void }) {
  const mine = turn.role === 'user'
  return (
    <div className={clsx('flex flex-col gap-2', mine ? 'items-end' : 'items-start')} data-testid={mine ? 'chat-user' : 'chat-assistant'}>
      <p
        className={clsx(
          'max-w-[90%] rounded-2xl px-4 py-2.5 text-sm leading-relaxed whitespace-pre-line',
          mine ? 'bg-accent/20' : 'bg-surface-2',
        )}
      >
        {turn.content}
      </p>
      {turn.actions?.map((a) => (
        <Link
          key={a.url}
          to="/play/$id"
          params={{ id: String(a.item_id) }}
          search={a.start_ms != null ? { start: a.start_ms } : {}}
          onClick={onNavigate}
          className="inline-flex items-center gap-1.5 rounded-full bg-accent px-4 py-2 text-xs font-bold text-accent-fg hover:bg-accent-hover"
        >
          <Play className="size-3.5 fill-current" aria-hidden="true" />
          {a.title} abspielen
        </Link>
      ))}
      {turn.items && turn.items.length > 0 && (
        <div className="flex max-w-full gap-2 overflow-x-auto pb-1">
          {turn.items.slice(0, 6).map((item) => (
            <Link
              key={item.id}
              to="/item/$id"
              params={{ id: String(item.id) }}
              onClick={onNavigate}
              className="w-20 shrink-0"
              title={item.title}
            >
              <BlurImage src={item.poster_url} blurhash={item.poster_blurhash} alt={item.title} className="aspect-[2/3] rounded-lg" />
              <span className="mt-1 block truncate text-[11px] text-muted">{item.title}</span>
            </Link>
          ))}
        </div>
      )}
    </div>
  )
}
