// First-run wizard: pick a template, a spec, or a hand-made tool, then copy the client command.

import { useEffect, useState, type FormEvent } from 'react'
import { ApiError, createServer, createToken, getTemplates, importApi } from './api'
import { ToolForm } from './Builder'
import { snippet, WHERE, type Client } from './clientCommand'
import { ClientTabs } from './ClientSnippet'
import { CopyButton } from './CopyButton'
import { useLiveStream } from './liveStream'
import { useSpecSource } from './specSource'
import { SpecSourceFields } from './SpecSourceFields'
import type { ImportApi } from './types/ImportApi'
import type { ImportedApi } from './types/ImportedApi'
import type { NewToken } from './types/NewToken'
import type { ServerSummary } from './types/ServerSummary'
import type { Template } from './types/Template'

// The same rule as `is_valid_server_name` in crates/core/src/server_name.rs.
const SERVER_NAME = /^[a-z0-9][a-z0-9-]{0,62}$/

/** Token names need not be unique. The date tells two onboarding tokens apart on the Tokens page. */
const tokenName = () => `first-client ${new Date().toISOString().slice(0, 10)}`

type Step = 'pick' | 'name' | 'build' | 'connect'
const STEPS: { id: Step; label: string }[] = [
  { id: 'pick', label: 'Pick a template' },
  { id: 'name', label: 'Name the server' },
  { id: 'connect', label: 'Copy the command' },
]

/** `own` is the card for the user's own OpenAPI spec. `hand` builds one tool in a form. */
type Choice = { kind: 'template'; template: Template } | { kind: 'own' } | { kind: 'hand' }

interface OnboardingProps {
  /** A server from an earlier, unfinished onboarding. The wizard continues with it. */
  existing?: ServerSummary
  /** Shown as a Cancel button when the user already has a working server. */
  onCancel?: () => void
  /** Reloads the server list. The wizard calls it after each change on the server. */
  onChanged: () => void
  onDone: (server: string) => void
}

export function Onboarding({ existing, onCancel, onChanged, onDone }: OnboardingProps) {
  const [step, setStep] = useState<Step>(existing && existing.apis > 0 ? 'connect' : 'pick')
  const [templates, setTemplates] = useState<Template[] | null>(null)
  const [choice, setChoice] = useState<Choice | null>(null)
  const [server, setServer] = useState<ServerSummary | null>(existing ?? null)
  const [imported, setImported] = useState<ImportedApi[]>([])
  const [token, setToken] = useState<NewToken | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    getTemplates().then(setTemplates, (e: Error) => setError(e.message))
  }, [])

  const pick = (next: Choice) => {
    setChoice(next)
    setError(null)
    setStep(next.kind === 'hand' ? 'build' : 'name')
  }

  const connect = async (created: ServerSummary) => {
    setServer(created)
    onChanged()
    try {
      setToken(await createToken(created.name, tokenName()))
    } catch (e) {
      // The connect step offers a button to create the token again.
      setError(e instanceof Error ? e.message : String(e))
    }
    setStep('connect')
  }

  return (
    <div className="onboarding">
      <div className="eyebrow">// {step === 'pick' ? 'Templates' : step === 'connect' ? 'Connect' : 'Your server'}</div>
      {step === 'pick' && (
        <>
          <h1 className="hero">Start from a template, not a blank form</h1>
          <p className="lead">
            A template is a ready-made API. You pick it, and your AI client can use its tools a few seconds later.
          </p>
          {error && <p className="alert" role="alert">{error}</p>}
          <div className="cards">
            {templates?.map((t) => (
              <button
                key={t.id}
                type="button"
                className={`card${choice?.kind === 'template' && choice.template.id === t.id ? ' selected' : ''}`}
                onClick={() => pick({ kind: 'template', template: t })}
              >
                <TemplateIcon id={t.id} />
                <span className="card-title">{t.title}</span>
                <span className="card-host">{t.host}</span>
                <span className="card-text">{t.summary}</span>
                <span className={`badge ${t.keyless ? 'ok' : 'key'}`}>{t.badge}</span>
              </button>
            ))}
            <button type="button" className="card" onClick={() => pick({ kind: 'hand' })}>
              <svg className="card-icon" width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M4 20h4L19 9l-4-4L4 16v4ZM13 7l4 4" />
              </svg>
              <span className="card-title">Build a tool by hand</span>
              <span className="card-host">no spec needed</span>
              <span className="card-text">Fill in one HTTP call. ToolPort turns it into an MCP tool.</span>
              <span className="badge key">your own API</span>
            </button>
          </div>
          <button type="button" className="link" onClick={() => pick({ kind: 'own' })}>
            Or import your own OpenAPI 3.0 spec →
          </button>
        </>
      )}

      {step === 'build' && (
        <>
          <h1 className="hero">Build your first tool</h1>
          {error && <p className="alert" role="alert">{error}</p>}
          <ToolForm
            server={null}
            initial={null}
            onboarding
            onSaved={(saved) => {
              if (saved.kind === 'server') void connect(saved.server)
            }}
          />
          <div className="actions">
            <button type="button" className="btn" onClick={() => setStep('pick')}>
              Back
            </button>
          </div>
        </>
      )}

      {step === 'name' && choice && choice.kind !== 'hand' && (
        <NameStep
          choice={choice}
          existing={server}
          onBack={() => setStep('pick')}
          onCreated={(created) => {
            setServer(created)
            onChanged()
          }}
          onImported={(result, newToken) => {
            setImported(result)
            setToken(newToken)
            setStep('connect')
            onChanged()
          }}
        />
      )}

      {step === 'connect' && server && (
        <ConnectStep
          server={server}
          imported={imported}
          token={token}
          onToken={(t) => {
            setToken(t)
            onChanged()
          }}
          onDone={() => onDone(server.name)}
        />
      )}

      <div className="steps-row">
        <ol className="steps" aria-label="Progress">
          {STEPS.map((s, i) => (
            <li key={s.id} aria-current={s.id === (step === 'build' ? 'name' : step) ? 'step' : undefined}>
              {i > 0 && <Arrow />}
              <span className="chip">{s.label}</span>
            </li>
          ))}
        </ol>
        <span className="eta">~30 s</span>
      </div>
      {onCancel && (
        <button type="button" className="link muted-link" onClick={onCancel}>
          Cancel and go back to the dashboard
        </button>
      )}
    </div>
  )
}

interface NameStepProps {
  choice: Exclude<Choice, { kind: 'hand' }>
  existing: ServerSummary | null
  onBack: () => void
  onCreated: (server: ServerSummary) => void
  onImported: (imported: ImportedApi[], token: NewToken) => void
}

function NameStep({ choice, existing, onBack, onCreated, onImported }: NameStepProps) {
  const [name, setName] = useState(existing?.name ?? (choice.kind === 'template' ? choice.template.server_name : ''))
  const spec = useSpecSource()
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const nameValid = SERVER_NAME.test(name)
  const specReady = choice.kind === 'template' || spec.ready

  const source = (): ImportApi =>
    choice.kind === 'template' ? { source: 'template', template: choice.template.id } : spec.source()

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    if (!nameValid || !specReady) return
    setError(null)
    try {
      // A server from a failed earlier try stays. The next try reuses it.
      if (!existing) {
        setBusy('Creating the server')
        onCreated(await createServer(name))
      }
      setBusy('Making the tools')
      const imported = await importApi(name, source())
      setBusy('Creating a token')
      const token = await createToken(name, tokenName())
      onImported(imported, token)
    } catch (e) {
      setError(e instanceof ApiError || e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(null)
    }
  }

  return (
    <form className="panel wizard" onSubmit={submit}>
      <h1 className="hero">{choice.kind === 'template' ? `Name your ${choice.template.title} server` : 'Import your API'}</h1>
      <label className="field">
        <span>Server name</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value.toLowerCase())}
          disabled={existing !== null || busy !== null}
          autoFocus
          spellCheck={false}
          aria-invalid={name !== '' && !nameValid}
        />
        <small className={name !== '' && !nameValid ? 'bad' : undefined}>
          Use lowercase letters, digits, and <code>-</code>. Your MCP URL ends with <code>/mcp/{name || '<name>'}</code>.
        </small>
      </label>

      {choice.kind === 'own' && <SpecSourceFields spec={spec} disabled={busy !== null} />}

      {error && <p className="alert" role="alert">{error}</p>}
      <div className="actions">
        <button type="button" className="btn" onClick={onBack} disabled={busy !== null}>
          Back
        </button>
        <button type="submit" className="btn primary big" disabled={!nameValid || !specReady || busy !== null}>
          {busy ? `${busy}…` : 'Create server'}
        </button>
      </div>
    </form>
  )
}

interface ConnectStepProps {
  server: ServerSummary
  imported: ImportedApi[]
  token: NewToken | null
  onToken: (token: NewToken) => void
  onDone: () => void
}

function ConnectStep({ server, imported, token, onToken, onDone }: ConnectStepProps) {
  const [client, setClient] = useState<Client>('Claude Code')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const tools = imported.flatMap((api) => api.tools)
  const toolCount = tools.length > 0 ? tools.length : server.tools
  const skipped = imported.reduce((n, api) => n + api.skipped.length, 0)

  const newToken = async () => {
    setBusy(true)
    setError(null)
    try {
      onToken(await createToken(server.name, tokenName()))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="panel wizard">
      <h1 className="hero">Connect your AI client</h1>
      {toolCount > 0 && (
        <p className="lead">
          <strong>{server.name}</strong> has {toolCount === 1 ? '1 tool' : `${toolCount} tools`}
          {tools.length > 0 && (
            <>
              : <span className="mono">{tools.join(', ')}</span>
            </>
          )}
          .
          {skipped > 0 && ` ToolPort skipped ${skipped} operations that it cannot call yet.`}
        </p>
      )}

      {token ? (
        <>
          <ClientTabs client={client} onChange={setClient} />
          <ConnectionBadge
            server={server.name}
            tokenId={token.row.id}
            command={snippet(client, server, token.token)}
            hint={client === 'Claude Code' ? 'Paste this into your terminal:' : WHERE[client]}
          />
          <p className="note">
            The command contains a token. ToolPort shows it only now and does not store it. The token works for{' '}
            {token.expires_in_days} days. You can revoke it on the Tokens page.
          </p>
        </>
      ) : (
        <>
          <p className="muted">Your client needs a token for this server. ToolPort shows the token value only once.</p>
          {error && <p className="alert" role="alert">{error}</p>}
          <div className="actions">
            <button type="button" className="btn primary big" onClick={newToken} disabled={busy}>
              {busy ? 'Creating the token…' : 'Create a token'}
            </button>
          </div>
        </>
      )}
      <div className="actions">
        <button type="button" className="btn big" onClick={onDone}>
          Open the Live page
        </button>
      </div>
    </section>
  )
}

/** One card with two states. It flips to CONNECTED on the first MCP request of a new client. */
function ConnectionBadge({ server, tokenId, command, hint }: { server: string; tokenId: number; command: string; hint: string }) {
  const live = useLiveStream(server, false)
  // Only a request with the token of this page counts. Names are not unique, so match the id.
  const first = live.firstRequests.find((r) => r.token_id === tokenId)
  // After a page reload, the snapshot still lists the client, but the first request event is gone.
  const known = live.clients.find((c) => c.token_id === tokenId)
  const client = first?.client_name ?? known?.client_name
  const connected = first !== undefined || known !== undefined
  const detail = first
    ? first.tools_sent === undefined
      ? first.method
      : `${first.method} · ${first.tools_sent} tools sent`
    : null

  return (
    <div className={`badge-card${connected ? ' connected' : ''}`} aria-live="polite">
      <div className="badge-state">
        <span className="badge-dot" aria-hidden="true" />
        {connected ? 'CONNECTED' : 'WAITING'}
      </div>
      {connected ? (
        <>
          <h2>Connected · {client ?? 'A client'}</h2>
          <p className="muted">First request received. Now ask Claude something.</p>
          {detail && <div className="badge-detail mono">{detail}</div>}
        </>
      ) : (
        <>
          <h2>Waiting for your agent…</h2>
          <p className="muted">{hint}</p>
          <div className="snippet">
            <pre>{command}</pre>
            <CopyButton text={command} label="Copy" className="btn primary" />
          </div>
        </>
      )}
    </div>
  )
}

function Arrow() {
  return (
    <svg className="arrow" width="30" height="10" viewBox="0 0 30 10" aria-hidden="true">
      <path d="M0 5h24M20 1l5 4-5 4" fill="none" stroke="currentColor" strokeWidth="2" />
    </svg>
  )
}

function TemplateIcon({ id }: { id: string }) {
  const common = { width: 26, height: 26, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 1.6, strokeLinecap: 'round', strokeLinejoin: 'round', 'aria-hidden': true } as const
  switch (id) {
    case 'weather':
      return (
        <svg className="card-icon" {...common}>
          <path d="M7 18a4 4 0 0 1-.6-7.96A5.5 5.5 0 0 1 17 9a3.5 3.5 0 0 1 .5 6.97V18H7Z" />
        </svg>
      )
    case 'github-issues':
      return (
        <svg className="card-icon" {...common}>
          <path d="m8 7-5 5 5 5M16 7l5 5-5 5M14 4l-4 16" />
        </svg>
      )
    default:
      return (
        <svg className="card-icon" {...common}>
          <rect x="3" y="5" width="18" height="15" rx="2" />
          <path d="M3 9h18M8 3v4M16 3v4M7 16l3-3 2 2 4-4" />
        </svg>
      )
  }
}
