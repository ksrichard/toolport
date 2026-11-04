import { useEffect, useState } from 'react'
import { CopyButton } from './CopyButton'
import { useCallLogAnimation, useListAnimation } from './motion'
import { BUCKET_MS, CHART_BUCKETS, useLiveStream, type LiveState, type StreamStatus } from './liveStream'
import type { CallResult } from './types/CallResult'
import type { ServerSummary } from './types/ServerSummary'
import { ToolSwitch } from './ToolSwitch'
import { useToolList } from './toolList'

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
}

export function Live({ server, onConnectClient, onOpenTool, onToolsChanged }: LiveProps) {
  const [paused, setPaused] = useState(false)
  const live = useLiveStream(server.name, paused)
  const now = useNow(BUCKET_MS)

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
          <section className="panel chart">
            <h2>
              Calls over time
              <small className="legend">
                <span>
                  <i />
                  calls / 5 s
                </span>
                <span>
                  <i className="e" />
                  errors
                </span>
              </small>
            </h2>
            <Chart buckets={live.buckets} now={now} />
          </section>
          <CallLog live={live} />
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
  return (
    <section className="panel kpis" aria-label="Metrics">
      <div className="kpi">
        <div className="label">Calls per minute</div>
        <div className="value">{m?.calls_per_minute ?? 0}</div>
        <div className="delta">last 60 s</div>
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

function Chart({ buckets, now }: { buckets: LiveState['buckets']; now: number }) {
  const W = 600
  const H = 120
  const pad = 18
  const last = Math.floor(now / BUCKET_MS) * BUCKET_MS
  const series = Array.from({ length: CHART_BUCKETS }, (_, i) => {
    const start = last - (CHART_BUCKETS - 1 - i) * BUCKET_MS
    return buckets.get(start) ?? { calls: 0, errors: 0 }
  })
  const max = Math.max(8, ...series.map((b) => b.calls))
  const x = (i: number) => (i / (CHART_BUCKETS - 1)) * W
  const y = (v: number) => H - pad - (v / max) * (H - pad - 6)
  const points = series.map((b, i) => `${x(i).toFixed(1)},${y(b.calls).toFixed(1)}`).join(' ')
  const mid = Math.round(max / 2)
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label="Calls per 5 seconds over the last 5 minutes">
      <line className="grid" x1="0" x2={W} y1={y(0)} y2={y(0)} />
      <line className="grid" x1="0" x2={W} y1={y(mid)} y2={y(mid)} strokeDasharray="2 4" />
      <text className="axis" x="4" y={y(mid) - 4}>
        {mid}
      </text>
      <text className="axis" x="4" y={H - 4}>
        5 min ago
      </text>
      <text className="axis" x={W - 4} y={H - 4} textAnchor="end">
        now
      </text>
      <polygon className="area" points={`0,${y(0)} ${points} ${W},${y(0)}`} />
      <polyline className="lineP" points={points} />
      {series.map((b, i) => (b.errors > 0 ? <circle key={i} className="errs" cx={x(i)} cy={y(b.calls)} r="3" /> : null))}
      <circle className="end" cx={x(CHART_BUCKETS - 1) - 3} cy={y(series[CHART_BUCKETS - 1].calls)} r="3.5" />
    </svg>
  )
}

function CallLog({ live }: { live: LiveState }) {
  const rowsRef = useCallLogAnimation()
  return (
    <section className="panel">
      <h2>
        Call log <small>newest first</small>
      </h2>
      {live.calls.length === 0 ? (
        <p className="muted">No calls yet. Ask your AI client to use one of the tools.</p>
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
            <tbody ref={rowsRef}>
              {live.calls.map((call) => {
                const result = RESULTS[call.result]
                return (
                  <tr key={call.id} className={live.fresh.has(call.id) ? 'new' : undefined}>
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
          <li key={t.name} className="tool">
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
              </div>
              <div>
                <span className="method">{t.method}</span> {t.path}
              </div>
            </div>
            <ToolSwitch tool={t} list={list} />
          </li>
        ))}
      </ul>
    </section>
  )
}
