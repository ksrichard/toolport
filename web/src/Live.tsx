import { useEffect, useState } from 'react'
import { CallsPanel } from './CallChart'
import { CopyButton } from './CopyButton'
import { useCallLogAnimation, useListAnimation } from './motion'
import { PixelScenes } from './PixelScenes'
import { getCallPage } from './api'
import { BUCKET_MS, MAX_CALLS, useLiveStream, type LiveState, type StreamStatus } from './liveStream'
import type { CallRecord } from './types/CallRecord'
import type { CallResult } from './types/CallResult'
import type { ServerSummary } from './types/ServerSummary'
import type { ToolSummary } from './types/ToolSummary'
import { usePlanBlocked } from './planLock'
import { ToolSwitch } from './ToolSwitch'
import { useToolList, type ToolList } from './toolList'
import { PausedPill } from './Tools'
import { usePlan } from './plans'

const STATUS_TEXT: Record<StreamStatus, string> = {
  connecting: 'Connecting',
  live: 'Live updates on',
  paused: 'Live updates paused',
  reconnecting: 'Reconnecting',
  failed: 'Live updates stopped',
}

const RESULTS: Record<CallResult, { label: string; tone: string }> = {
  ok: { label: 'OK', tone: 'ok' },
  not_found: { label: 'Not found', tone: 'warn' },
  upstream_error: { label: 'Upstream error', tone: 'bad' },
  timed_out: { label: 'Timed out', tone: 'bad' },
}

/** Re-renders the component every `ms` milliseconds, so the chart window and the "ago" texts update. */
function useNow(ms: number): number {
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), ms)
    return () => clearInterval(id)
  }, [ms])
  return now
}

/** Local date and time, for example 2026-11-04 14:03:22. */
const dateTime = (ms: number) => {
  const d = new Date(ms)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${d.toTimeString().slice(0, 8)}`
}

function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return `${s} s`
  if (s < 3600) return `${Math.round(s / 60)} min`
  return `${Math.round(s / 3600)} h`
}

interface LiveProps {
  server: ServerSummary
  onConnectClient: () => void
  /** Opens the builder. Null opens it for a new tool. */
  onOpenTool: (tool: string | null) => void
  onToolsChanged?: () => void
  /** Opens a page of the app, for example /billing. */
  navigate: (path: string) => void
}

export function Live(props: LiveProps) {
  return props.server.paused ? <PausedLive server={props.server} navigate={props.navigate} /> : <ActiveLive {...props} />
}

const EMPTY_LIVE: LiveState = {
  status: 'paused',
  error: null,
  metrics: null,
  clients: [],
  buckets: new Map(),
  calls: [],
  fresh: new Set(),
  firstRequests: [],
}

/**
 * A paused server does not serve MCP requests, so the page opens no live stream. It shows the
 * page layout without data and links to the pages that can end the pause.
 */
function PausedLive({ server, navigate }: { server: ServerSummary; navigate: (path: string) => void }) {
  const { billing } = usePlan()
  const plan = billing?.plan
  const limit = plan?.max_servers
  const reason =
    plan && limit != null
      ? `The ${plan.name} plan allows ${limit === 1 ? '1 server' : `${limit} servers`}, and this server is over the limit.`
      : 'This server is over the plan limit.'
  return (
    <>
      <div className="head">
        <div>
          <h1>{server.name}</h1>
          <div className="sub">
            <span className="stream paused">
              <span className="dot" />
              Server paused
            </span>
          </div>
        </div>
        <div className="head-actions">
          <div className="endpoint is-disabled">
            <code>{server.mcp_url}</code>
          </div>
        </div>
      </div>

      <section className="panel callout" role="status">
        <div>
          <h2>This server is paused</h2>
          <p className="muted">
            {reason} MCP clients get an error, and ToolPort does not monitor the server. Upgrade your plan, or delete
            another server to start this one again.
          </p>
        </div>
        <div className="row-actions">
          <button className="btn primary" type="button" onClick={() => navigate('/billing')}>
            Go to Plan &amp; billing
          </button>
          <button className="btn" type="button" onClick={() => navigate('/servers')}>
            Manage servers
          </button>
        </div>
      </section>

      <div className="paused-view" inert aria-hidden="true">
        <Kpis live={EMPTY_LIVE} />
        <div className="grid2">
          <div className="col">
            <section className="panel">
              <h2>Calls over time</h2>
              <p className="muted">No monitoring while the server is paused.</p>
            </section>
            <section className="panel">
              <h2>Call log</h2>
              <p className="muted">No monitoring while the server is paused.</p>
            </section>
          </div>
          <div className="col">
            <section className="panel">
              <h2>Connected clients</h2>
              <p className="muted">No client can connect while the server is paused.</p>
            </section>
          </div>
        </div>
      </div>
    </>
  )
}

function ActiveLive({ server, onConnectClient, onOpenTool, onToolsChanged }: LiveProps) {
  const [paused, setPaused] = useState(false)
  const live = useLiveStream(server.name, paused)
  const now = useNow(BUCKET_MS)
  const newest = live.calls[0]
  const hint =
    newest && live.fresh.has(newest.id) ? { id: newest.id, tool: newest.tool, failed: newest.result !== 'ok' } : null

  return (
    <>
      <div className="head">
        <div>
          <h1>{server.name}</h1>
          <div className="sub">
            {server.apis === 1 ? '1 API' : `${server.apis} APIs`} bridged to MCP.{' '}
            <span className={`stream ${live.status}`}>
              <span className="dot" />
              {STATUS_TEXT[live.status]}
            </span>
          </div>
        </div>
        <PixelScenes paused={paused} hint={hint} />
        <div className="head-actions">
          <div className="endpoint">
            <code>{server.mcp_url}</code>
            <CopyButton text={server.mcp_url} label="Copy URL" />
          </div>
          <button className="btn" type="button" onClick={() => setPaused((p) => !p)}>
            {paused ? 'Resume' : 'Pause'}
          </button>
        </div>
      </div>

      {live.error && (
        <p className="alert" role="alert">
          {live.error}
        </p>
      )}

      {server.active_tokens === 0 && (
        <section className="panel callout">
          <div>
            <h2>No client can connect yet</h2>
            <p className="muted">This server has no active token. Create one and copy the command for your MCP client.</p>
          </div>
          <button className="btn primary" type="button" onClick={onConnectClient}>
            Connect a client
          </button>
        </section>
      )}

      <Kpis live={live} />

      <div className="grid2">
        <div className="col">
          <CallsPanel server={server.name} live={live} now={now} paused={paused} />
          <CallLog server={server.name} live={live} />
        </div>
        <div className="col">
          <Clients live={live} now={now} />
          <Tools server={server.name} onOpenTool={onOpenTool} onToolsChanged={onToolsChanged} />
        </div>
      </div>
    </>
  )
}

function Kpis({ live }: { live: LiveState }) {
  const m = live.metrics
  const { billing } = usePlan()
  const max = billing?.plan.max_calls_per_month
  return (
    <section className="panel kpis" aria-label="Metrics">
      <div className="kpi">
        <div className="label">Calls per minute</div>
        <div className="value">{m?.calls_per_minute ?? 0}</div>
        <div className="delta">
          last 60 s
          {billing && max != null && ` · this month ${billing.calls_used} / ${max}`}
        </div>
      </div>
      <div className="kpi">
        <div className="label">Error rate</div>
        <div className="value">
          {((m?.error_rate ?? 0) * 100).toFixed(1)}
          <small>%</small>
        </div>
        <div className="delta">non-2xx and timeouts</div>
      </div>
      <div className="kpi">
        <div className="label">p95 latency</div>
        <div className="value">
          {m?.p95_ms ?? '–'}
          <small>ms</small>
        </div>
        <div className="delta">p50 {m?.p50_ms ?? '–'} ms</div>
      </div>
      <div className="kpi">
        <div className="label">Connected clients</div>
        <div className="value">{live.clients.length}</div>
        <div className="delta">active in the last 2 min</div>
      </div>
    </section>
  )
}

const PAGE_SIZES = [10, 20, 50]
const DEFAULT_ROWS = 20

/** The browser remembers the page size. It is a convenience only, so errors are ignored. */
const ROWS_KEY = 'toolport.callLogRows'
function loadRows(): number {
  try {
    const saved = Number(localStorage.getItem(ROWS_KEY))
    return PAGE_SIZES.includes(saved) ? saved : DEFAULT_ROWS
  } catch {
    return DEFAULT_ROWS
  }
}
function saveRows(rows: number) {
  try {
    localStorage.setItem(ROWS_KEY, String(rows))
  } catch {
    // Private windows can block storage.
  }
}

/** An older page. It does not change after it loads. */
interface OlderPage {
  calls: CallRecord[]
  hasMore: boolean
}

/** Page 1 shows the newest calls from the SSE stream. Older pages load from the call route. */
function CallLog({ server, live }: { server: string; live: LiveState }) {
  const rowsRef = useCallLogAnimation()
  const [rows, setRows] = useState(loadRows)
  // The older pages that the user opened, page 2 first.
  const [older, setOlder] = useState<OlderPage[]>([])
  // The newest call id when the user left page 1. Newer calls count as new.
  const [leftAt, setLeftAt] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const first = live.calls.slice(0, rows)
  // The stream keeps MAX_CALLS calls. When it holds that many, the server can have more.
  const firstHasMore = live.calls.length > rows || live.calls.length >= MAX_CALLS
  const page = older.length + 1
  const current = older.at(-1)
  const shown = current ? current.calls : first
  const hasMore = current ? current.hasMore : firstHasMore
  const newCalls = leftAt === null ? 0 : live.calls.filter((c) => c.id > leftAt).length

  const backToLatest = () => {
    setOlder([])
    setLeftAt(null)
    setError(null)
  }

  const chooseRows = (value: number) => {
    setRows(value)
    saveRows(value)
    backToLatest()
  }

  const loadOlder = async () => {
    const last = shown.at(-1)
    if (!last) return
    setBusy(true)
    setError(null)
    try {
      const next = await getCallPage(server, rows, last.id)
      if (page === 1) setLeftAt(live.calls[0]?.id ?? null)
      setOlder((list) => [...list, { calls: next.calls, hasMore: next.has_more }])
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const newer = () => {
    if (page === 2) backToLatest()
    else setOlder((list) => list.slice(0, -1))
  }

  const from = (page - 1) * rows + 1
  const range = shown.length === 0 ? `Page ${page}` : `Calls ${from}–${from + shown.length - 1}`

  return (
    <section className="panel call-log">
      <h2>
        <span>
          Call log <small>newest first</small>
        </span>
        <label className="rows-pick">
          <small>Rows</small>
          <select value={rows} onChange={(e) => chooseRows(Number(e.target.value))}>
            {PAGE_SIZES.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
      </h2>
      {error && (
        <p className="alert" role="alert">
          Could not load the calls: {error}
        </p>
      )}
      {newCalls > 0 && (
        <p className="new-calls">
          {newCalls >= MAX_CALLS ? `${MAX_CALLS}+` : newCalls} new {newCalls === 1 ? 'call' : 'calls'}.{' '}
          <button className="link" type="button" onClick={backToLatest}>
            Back to the latest
          </button>
        </p>
      )}
      {shown.length === 0 ? (
        <p className="muted">
          {page === 1 ? 'No calls yet. Ask your AI client to use one of the tools.' : 'No older calls.'}
        </p>
      ) : (
        <div className="scroll">
          <table>
            <thead>
              <tr>
                <th>Date and time</th>
                <th>Tool</th>
                <th>Client</th>
                <th>Status</th>
                <th className="num">HTTP</th>
                <th className="num">Latency</th>
              </tr>
            </thead>
            {/* A new key per page, so a page change does not animate every row. */}
            <tbody key={page} ref={page === 1 ? rowsRef : undefined}>
              {shown.map((call) => {
                const result = RESULTS[call.result]
                return (
                  <tr key={call.id} className={page === 1 && live.fresh.has(call.id) ? 'new' : undefined}>
                    <td className="mono">{dateTime(call.at_ms)}</td>
                    <td className="mono">{call.tool}</td>
                    <td>{call.client_name ?? call.token_name ?? '–'}</td>
                    <td>
                      <span className={`pill ${result.tone}`}>{result.label}</span>
                    </td>
                    <td className="num mono">{call.http_status ?? '–'}</td>
                    <td className="num mono">{call.latency_ms.toLocaleString()} ms</td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
      {(page > 1 || hasMore) && (
        <div className="pager">
          <button
            className="btn arrow"
            type="button"
            onClick={newer}
            disabled={page === 1 || busy}
            aria-label="Newer calls"
            title="Newer calls"
          >
            ←
          </button>
          <span className="muted">{range}</span>
          <button
            className="btn arrow"
            type="button"
            onClick={loadOlder}
            disabled={!hasMore || busy || shown.length === 0}
            aria-label="Older calls"
            aria-busy={busy}
            title="Older calls"
          >
            →
          </button>
        </div>
      )}
    </section>
  )
}

function Clients({ live, now }: { live: LiveState; now: number }) {
  const listRef = useListAnimation<HTMLUListElement>()
  return (
    <section className="panel">
      <h2>
        Connected clients <small>{live.clients.length} active</small>
      </h2>
      {live.clients.length === 0 ? (
        <p className="muted">No MCP client sent a request in the last 2 minutes.</p>
      ) : (
        <ul className="list" ref={listRef}>
          {live.clients.map((c) => (
            <li key={`${c.token_name}/${c.client_name}`}>
              <div className="name">{c.client_name ?? 'Unknown client'}</div>
              <div className="meta">token {c.token_name}</div>
              <div className="side-val">{ago(now - c.since_ms)}</div>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function Tools({
  server,
  onOpenTool,
  onToolsChanged,
}: {
  server: string
  onOpenTool: (tool: string | null) => void
  onToolsChanged?: () => void
}) {
  const list = useToolList(server, onToolsChanged)
  const error = list.loadError ?? list.switchError
  const listRef = useListAnimation<HTMLUListElement>()

  return (
    <section className="panel">
      <h2>
        Tools{' '}
        <button className="btn" type="button" onClick={() => onOpenTool(null)}>
          New tool
        </button>
      </h2>
      {error && (
        <p className="alert" role="alert">
          {error}
        </p>
      )}
      <ul className="list" ref={listRef}>
        {list.tools?.map((t) => (
          <LiveToolRow key={t.name} tool={t} list={list} server={server} onOpenTool={onOpenTool} />
        ))}
      </ul>
    </section>
  )
}

interface LiveToolRowProps {
  tool: ToolSummary
  list: ToolList
  server: string
  onOpenTool: (tool: string | null) => void
}

function LiveToolRow({ tool: t, list, server, onOpenTool }: LiveToolRowProps) {
  const blocked = usePlanBlocked(t, list)
  return (
    <li className={blocked ? 'tool is-paused' : 'tool'}>
      <div className="t">
        <div>
          <a
            href={`/servers/${encodeURIComponent(server)}/tools/${encodeURIComponent(t.name)}`}
            onClick={(e) => {
              e.preventDefault()
              onOpenTool(t.name)
            }}
          >
            {t.name}
          </a>
          {t.source === 'manual' && <span className="method">manual</span>}
          {t.edited && <span className="method">edited</span>}
          {blocked && <PausedPill />}
        </div>
        <div>
          <span className="method">{t.method}</span> {t.path}
        </div>
      </div>
      <ToolSwitch tool={t} list={list} />
    </li>
  )
}
