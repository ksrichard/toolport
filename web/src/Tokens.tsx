// Tokens page: the MCP client tokens of the server. A new token value shows only once.

import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { ApiError, createToken, getTokens, revokeToken } from './api'
import { ClientSnippet } from './ClientSnippet'
import { CopyButton } from './CopyButton'
import { useListAnimation } from './motion'
import type { NewToken } from './types/NewToken'
import type { ServerSummary } from './types/ServerSummary'
import type { TokenView } from './types/TokenView'

// The same limit as `MAX_NAME_CHARS` in crates/server/src/mcp_tokens.rs.
const MAX_NAME_CHARS = 64

const message = (error: unknown) => (error instanceof ApiError || error instanceof Error ? error.message : String(error))

function relative(ms: number, now: number): string {
  const s = Math.max(0, Math.round((now - ms) / 1000))
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.round(s / 60)} min ago`
  if (s < 86_400) return `${Math.round(s / 3600)} h ago`
  const days = Math.round(s / 86_400)
  return days === 1 ? '1 day ago' : `${days} days ago`
}

function status(token: TokenView, now: number): { label: string; tone: string } {
  if (token.revoked_ms !== null) return { label: 'revoked', tone: 'bad' }
  if (token.expires_ms <= now) return { label: 'expired', tone: 'warn' }
  return { label: 'active', tone: 'ok' }
}

interface TokensProps {
  server: ServerSummary
  /** Reloads the server list, because the token count in the side menu changes. */
  onChanged: () => void
}

export function Tokens({ server, onChanged }: TokensProps) {
  const [tokens, setTokens] = useState<TokenView[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const [created, setCreated] = useState<NewToken | null>(null)
  const [now, setNow] = useState(Date.now)
  const rowsRef = useListAnimation<HTMLTableSectionElement>()

  const load = useCallback(() => {
    getTokens(server.name).then(
      (list) => {
        setTokens(list)
        setNow(Date.now())
        setLoadError(null)
      },
      (e: unknown) => setLoadError(message(e)),
    )
  }, [server.name])
  useEffect(load, [load])

  const replace = (token: TokenView) => {
    setTokens((list) => list?.map((t) => (t.id === token.id ? token : t)) ?? null)
    setNow(Date.now())
    onChanged()
  }

  return (
    <>
      <div className="head">
        <div>
          <h1>Tokens</h1>
          <div className="sub">An MCP client sends a token to connect. A revoked token stops working at once.</div>
        </div>
        <div className="head-actions">
          {!creating && (
            <button
              className="btn primary"
              type="button"
              onClick={() => {
                setCreated(null)
                setCreating(true)
              }}
            >
              Create token
            </button>
          )}
        </div>
      </div>

      {loadError && (
        <p className="alert" role="alert">
          Could not load the tokens: {loadError}
        </p>
      )}
      {creating && (
        <CreateToken
          server={server.name}
          onCancel={() => setCreating(false)}
          onCreated={(token) => {
            setCreating(false)
            setCreated(token)
            setTokens((list) => [token.row, ...(list ?? [])])
            setNow(Date.now())
            onChanged()
          }}
        />
      )}
      {created && <CreatedToken server={server} token={created} onClose={() => setCreated(null)} />}

      <section className="panel">
        {tokens?.length === 0 ? (
          <p className="muted">This server has no tokens yet.</p>
        ) : (
          <div className="scroll">
            <table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Created</th>
                  <th>Last use</th>
                  <th>Status</th>
                  <th />
                </tr>
              </thead>
              <tbody ref={rowsRef}>
                {tokens?.map((token) => (
                  <TokenRow key={token.id} server={server.name} token={token} now={now} onRevoked={replace} />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </>
  )
}

interface TokenRowProps {
  server: string
  token: TokenView
  now: number
  onRevoked: (token: TokenView) => void
}

function TokenRow({ server, token, now, onRevoked }: TokenRowProps) {
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const state = status(token, now)

  const revoke = async () => {
    setBusy(true)
    setError(null)
    try {
      onRevoked(await revokeToken(server, token.id))
      setConfirm(false)
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <tr>
      <td className="mono">{token.name}</td>
      <td title={new Date(token.created_ms).toLocaleString()}>{new Date(token.created_ms).toLocaleDateString()}</td>
      <td title={token.last_used_ms === null ? undefined : new Date(token.last_used_ms).toLocaleString()}>
        {token.last_used_ms === null ? <span className="muted">never</span> : relative(token.last_used_ms, now)}
      </td>
      <td>
        <span className={`pill ${state.tone}`}>{state.label}</span>
      </td>
      <td className="num">
        {error && (
          <span className="cell-error" role="alert">
            {error}{' '}
          </span>
        )}
        {state.label === 'active' &&
          (confirm ? (
            <span className="line confirm">
              <span className="muted">Clients with this token lose access.</span>
              <button className="btn" type="button" onClick={() => setConfirm(false)} disabled={busy}>
                Cancel
              </button>
              <button className="btn danger" type="button" onClick={revoke} disabled={busy}>
                {busy ? 'Revoking…' : 'Yes, revoke'}
              </button>
            </span>
          ) : (
            <button className="btn" type="button" onClick={() => setConfirm(true)} aria-label={`Revoke ${token.name}`}>
              Revoke
            </button>
          ))}
      </td>
    </tr>
  )
}

interface CreateTokenProps {
  server: string
  onCancel: () => void
  onCreated: (token: NewToken) => void
}

function CreateToken({ server, onCancel, onCreated }: CreateTokenProps) {
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const trimmed = name.trim()
  const tooLong = [...trimmed].length > MAX_NAME_CHARS

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    if (trimmed === '' || tooLong) return
    setBusy(true)
    setError(null)
    try {
      onCreated(await createToken(server, trimmed))
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className="panel wizard" onSubmit={submit}>
      <h2>Create token</h2>
      <label className="field">
        <span>Name</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="claude-code-laptop"
          disabled={busy}
          autoFocus
          spellCheck={false}
          aria-invalid={tooLong}
        />
        <small className={tooLong ? 'bad' : undefined}>
          Name the client or the device that uses the token. Use at most {MAX_NAME_CHARS} characters.
        </small>
      </label>
      {error && (
        <p className="alert" role="alert">
          {error}
        </p>
      )}
      <div className="actions">
        <button type="button" className="btn" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
        <button type="submit" className="btn primary" disabled={trimmed === '' || tooLong || busy}>
          {busy ? 'Creating…' : 'Create'}
        </button>
      </div>
    </form>
  )
}

function CreatedToken({ server, token, onClose }: { server: ServerSummary; token: NewToken; onClose: () => void }) {
  return (
    <section className="panel wizard" aria-live="polite">
      <h2>
        Token {token.row.name} <small>works for {token.expires_in_days} days</small>
      </h2>
      <p className="alert">Copy the token now. ToolPort does not store it and cannot show it again.</p>
      <div className="snippet">
        <pre>{token.token}</pre>
        <CopyButton text={token.token} label="Copy token" className="btn primary" />
      </div>
      <ClientSnippet server={server} token={token.token} />
      <div className="actions">
        <button type="button" className="btn" onClick={onClose}>
          Done
        </button>
      </div>
    </section>
  )
}
