// All servers page: the user's MCP servers with a short status. Rename and delete live here.

import { Fragment, useEffect, useState, type FormEvent } from 'react'
import { ApiError, deleteServer, renameServer } from './api'
import { useListAnimation } from './motion'
import { PixelScenes } from './PixelScenes'
import { isValidServerName } from './serverName'
import { relative } from './time'
import type { ServerSummary } from './types/ServerSummary'

const message = (error: unknown) => (error instanceof ApiError || error instanceof Error ? error.message : String(error))

// A client counts as connected for 2 minutes after its last request, so a slower refresh is enough.
const REFRESH_MS = 30_000

const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? '' : 's'}`

function status(server: ServerSummary): { label: string; tone: string } {
  if (server.apis === 0) return { label: 'setup not done', tone: 'warn' }
  if (server.active_tokens === 0) return { label: 'no active token', tone: 'warn' }
  if (server.connected_clients > 0) return { label: `${plural(server.connected_clients, 'client')} connected`, tone: 'ok' }
  return { label: 'ready', tone: 'ok' }
}

interface ServersProps {
  servers: ServerSummary[]
  selected: string
  onOpen: (name: string) => void
  onNew: () => void
  onRenamed: (oldName: string, newName: string) => void
  onDeleted: (name: string) => void
  /** Reloads the server list, because the connected client counts change over time. */
  onRefresh: () => void
}

export function Servers({ servers, selected, onOpen, onNew, onRenamed, onDeleted, onRefresh }: ServersProps) {
  const [now, setNow] = useState(Date.now)
  const [open, setOpen] = useState<{ name: string; form: 'rename' | 'delete' } | null>(null)
  const rowsRef = useListAnimation<HTMLTableSectionElement>()

  useEffect(() => {
    onRefresh()
    const timer = window.setInterval(() => {
      onRefresh()
      setNow(Date.now())
    }, REFRESH_MS)
    return () => window.clearInterval(timer)
  }, [onRefresh])

  return (
    <>
      <div className="head">
        <div>
          <h1>All servers</h1>
          <div className="sub">Each server has its own MCP URL, APIs, tools, and tokens.</div>
        </div>
        <PixelScenes />
        <div className="head-actions">
          <button className="btn primary" type="button" onClick={onNew}>
            New server
          </button>
        </div>
      </div>

      <section className="panel">
        <div className="scroll">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Status</th>
                <th className="num">APIs</th>
                <th className="num">Tools on</th>
                <th className="num">Tokens</th>
                <th>Last call</th>
                <th />
              </tr>
            </thead>
            <tbody ref={rowsRef}>
              {servers.map((server) => {
                const state = status(server)
                const form = open?.name === server.name ? open.form : null
                return (
                  <Fragment key={server.name}>
                    <tr>
                      <td className="mono">
                        <a
                          href="/"
                          onClick={(event) => {
                            event.preventDefault()
                            onOpen(server.name)
                          }}
                          aria-current={server.name === selected ? 'true' : undefined}
                        >
                          {server.name}
                        </a>
                        {server.name === selected && <span className="muted"> (selected)</span>}
                      </td>
                      <td>
                        <span className={`pill ${state.tone}`}>{state.label}</span>
                      </td>
                      <td className="num">{server.apis}</td>
                      <td className="num">
                        {server.enabled_tools}
                        <span className="muted"> / {server.tools}</span>
                      </td>
                      <td className="num">{server.active_tokens}</td>
                      <td title={server.last_call_ms === null ? undefined : new Date(server.last_call_ms).toLocaleString()}>
                        {server.last_call_ms === null ? <span className="muted">never</span> : relative(server.last_call_ms, now)}
                      </td>
                      <td className="num">
                        <span className="line">
                          <button
                            className="btn"
                            type="button"
                            onClick={() => setOpen({ name: server.name, form: 'rename' })}
                            disabled={form !== null}
                            aria-label={`Rename ${server.name}`}
                          >
                            Rename
                          </button>
                          <button
                            className="btn"
                            type="button"
                            onClick={() => setOpen({ name: server.name, form: 'delete' })}
                            disabled={form !== null}
                            aria-label={`Delete ${server.name}`}
                          >
                            Delete
                          </button>
                        </span>
                      </td>
                    </tr>
                    {form && (
                      <tr>
                        <td colSpan={7} className="form-cell">
                          {form === 'rename' ? (
                            <RenameForm
                              server={server}
                              onCancel={() => setOpen(null)}
                              onDone={(newName) => {
                                setOpen(null)
                                onRenamed(server.name, newName)
                              }}
                            />
                          ) : (
                            <DeleteForm
                              server={server}
                              onCancel={() => setOpen(null)}
                              onDone={() => {
                                setOpen(null)
                                onDeleted(server.name)
                              }}
                            />
                          )}
                        </td>
                      </tr>
                    )}
                  </Fragment>
                )
              })}
            </tbody>
          </table>
        </div>
      </section>
    </>
  )
}

interface FormProps {
  server: ServerSummary
  onCancel: () => void
}

function RenameForm({ server, onCancel, onDone }: FormProps & { onDone: (newName: string) => void }) {
  const [name, setName] = useState(server.name)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const valid = isValidServerName(name)
  const changed = name !== server.name
  const newUrl = server.mcp_url.slice(0, server.mcp_url.length - server.name.length) + (name || '<name>')

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    if (!valid || !changed) return
    setBusy(true)
    setError(null)
    try {
      onDone((await renameServer(server.name, name)).name)
    } catch (e) {
      setError(message(e))
      setBusy(false)
    }
  }

  return (
    <form className="wizard confirm" onSubmit={submit}>
      <label className="field">
        <span>New name for {server.name}</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value.toLowerCase())}
          disabled={busy}
          autoFocus
          spellCheck={false}
          aria-invalid={name !== '' && !valid}
        />
        <small className={name !== '' && !valid ? 'bad' : undefined}>
          Use lowercase letters, digits, and <code>-</code>. The new MCP URL is <code>{newUrl}</code>.
        </small>
        <small className="warn" role="note">
          The old URL stops working at once. Open MCP sessions close. Each client needs the new URL. The tokens stay valid.
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
        <button type="submit" className="btn primary" disabled={!valid || !changed || busy}>
          {busy ? 'Renaming…' : 'Rename'}
        </button>
      </div>
    </form>
  )
}

function DeleteForm({ server, onCancel, onDone }: FormProps & { onDone: () => void }) {
  const [typed, setTyped] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const matches = typed.trim() === server.name

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    if (!matches) return
    setBusy(true)
    setError(null)
    try {
      await deleteServer(server.name)
      onDone()
    } catch (e) {
      setError(message(e))
      setBusy(false)
    }
  }

  return (
    <form className="wizard confirm" onSubmit={submit}>
      <p className="alert">
        Delete {server.name} with its {plural(server.apis, 'API')}, {plural(server.tools, 'tool')},{' '}
        {plural(server.active_tokens, 'active token')}, and its call log? Clients of this server lose access at once. You
        cannot undo this.
      </p>
      <label className="field">
        <span>
          Type <code>{server.name}</code> to confirm
        </span>
        <input
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          disabled={busy}
          autoFocus
          spellCheck={false}
          autoComplete="off"
        />
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
        <button type="submit" className="btn danger" disabled={!matches || busy}>
          {busy ? 'Deleting…' : 'Delete server'}
        </button>
      </div>
    </form>
  )
}
