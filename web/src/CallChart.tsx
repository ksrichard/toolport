// The "Calls over time" panel of the Live page. The 5 min window uses the 5 s buckets of the
// SSE stream. The longer windows load their buckets from GET /api/servers/{name}/calls/series.

import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { getCallSeries } from './api'
import { BUCKET_MS, CHART_BUCKETS, type LiveState } from './liveStream'
import type { SeriesWindow } from './types/SeriesWindow'

export type ChartWindow = '5m' | SeriesWindow

const WINDOWS: { id: ChartWindow; label: string; bucket: string }[] = [
  { id: '5m', label: '5 min', bucket: '5 s' },
  { id: '1h', label: '1 h', bucket: '1 min' },
  { id: '24h', label: '24 h', bucket: '15 min' },
  { id: '7d', label: '7 d', bucket: '1 h' },
  { id: '30d', label: '30 d', bucket: '6 h' },
]

/** A long window refreshes on this timer, and also soon after new calls. */
const REFRESH_MS = 30_000
const CALL_REFRESH_MS = 3_000

interface Point {
  start: number
  calls: number
  errors: number
}

interface Series {
  bucketMs: number
  points: Point[]
}

/** The browser remembers the window. It is a convenience only, so errors are ignored. */
const WINDOW_KEY = 'toolport.chartWindow'
function loadWindow(): ChartWindow {
  try {
    const saved = localStorage.getItem(WINDOW_KEY)
    return WINDOWS.find((w) => w.id === saved)?.id ?? '5m'
  } catch {
    return '5m'
  }
}
function saveWindow(window: ChartWindow) {
  try {
    localStorage.setItem(WINDOW_KEY, window)
  } catch {
    // Private windows can block storage.
  }
}

/** Loads the series of a long window. It returns null for the 5 min window and while it loads. */
function useLongSeries(server: string, window: ChartWindow, paused: boolean, newestCallId: number | undefined) {
  const [data, setData] = useState<{ key: string; series: Series } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [tick, setTick] = useState(0)
  const lastFetch = useRef(0)
  const seenCall = useRef(newestCallId)
  const key = `${server}/${window}`

  useEffect(() => {
    if (window === '5m') return
    let cancelled = false
    getCallSeries(server, window)
      .then((s) => {
        if (cancelled) return
        lastFetch.current = Date.now()
        const points = s.buckets.map((b) => ({ start: b.start_ms, calls: b.calls, errors: b.errors }))
        setData({ key: `${server}/${window}`, series: { bucketMs: s.bucket_secs * 1000, points } })
        setError(null)
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : 'Cannot load the chart data.')
      })
    return () => {
      cancelled = true
    }
  }, [server, window, tick])

  useEffect(() => {
    if (window === '5m' || paused) return
    const id = setInterval(() => setTick((t) => t + 1), REFRESH_MS)
    return () => clearInterval(id)
  }, [window, paused])

  // The wait counts from the last load, so a steady flow of calls still gives one load each 3 s.
  useEffect(() => {
    if (window === '5m' || paused || newestCallId === seenCall.current) return
    const wait = Math.max(0, lastFetch.current + CALL_REFRESH_MS - Date.now())
    const id = setTimeout(() => {
      seenCall.current = newestCallId
      setTick((t) => t + 1)
    }, wait)
    return () => clearTimeout(id)
  }, [window, paused, newestCallId])

  return { series: data?.key === key ? data.series : null, error: window === '5m' ? null : error }
}

function liveSeries(buckets: LiveState['buckets'], now: number): Series {
  const last = Math.floor(now / BUCKET_MS) * BUCKET_MS
  const points = Array.from({ length: CHART_BUCKETS }, (_, i) => {
    const start = last - (CHART_BUCKETS - 1 - i) * BUCKET_MS
    return { start, ...(buckets.get(start) ?? { calls: 0, errors: 0 }) }
  })
  return { bucketMs: BUCKET_MS, points }
}

const pad = (n: number) => String(n).padStart(2, '0')
const clock = (ms: number, seconds: boolean) => {
  const d = new Date(ms)
  return `${pad(d.getHours())}:${pad(d.getMinutes())}${seconds ? `:${pad(d.getSeconds())}` : ''}`
}
const day = (ms: number) => new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
const usesDays = (window: ChartWindow) => window === '7d' || window === '30d'

function tickLabel(ms: number, window: ChartWindow): string {
  return usesDays(window) ? day(ms) : clock(ms, window === '5m')
}

function rangeLabel(p: Point, bucketMs: number, window: ChartWindow): string {
  const end = p.start + bucketMs
  const seconds = window === '5m'
  if (!usesDays(window)) return `${clock(p.start, seconds)}–${clock(end, seconds)}`
  const endDay = day(end) === day(p.start) ? '' : `${day(end)} `
  return `${day(p.start)} ${clock(p.start, false)}–${endDay}${clock(end, false)}`
}

const count = (n: number, word: string) => `${n.toLocaleString()} ${word}${n === 1 ? '' : 's'}`

interface CallsPanelProps {
  server: string
  live: LiveState
  now: number
  paused: boolean
}

export function CallsPanel({ server, live, now, paused }: CallsPanelProps) {
  const [window, setWindow] = useState<ChartWindow>(loadWindow)
  const long = useLongSeries(server, window, paused, live.calls[0]?.id)
  const info = WINDOWS.find((w) => w.id === window) ?? WINDOWS[0]
  const series = window === '5m' ? liveSeries(live.buckets, now) : long.series

  const choose = (id: ChartWindow) => {
    setWindow(id)
    saveWindow(id)
  }

  return (
    <section className="panel chart">
      <h2>
        Calls over time
        <span className="chart-tools">
          <small className="legend">
            <span>
              <i />
              calls / {info.bucket}
            </span>
            <span>
              <i className="e" />
              errors
            </span>
          </small>
          <span className="skins seg" role="group" aria-label="Time window">
            {WINDOWS.map((w) => (
              <button key={w.id} type="button" aria-pressed={w.id === window} onClick={() => choose(w.id)}>
                {w.label}
              </button>
            ))}
          </span>
        </span>
      </h2>
      {long.error && (
        <p className="alert" role="alert">
          {long.error}
        </p>
      )}
      {series ? (
        <Chart series={series} window={window} span={info.label} bucket={info.bucket} />
      ) : (
        <div className="chart-plot chart-loading muted">Loading…</div>
      )}
    </section>
  )
}

interface ChartProps {
  series: Series
  window: ChartWindow
  span: string
  bucket: string
}

function Chart({ series, window, span, bucket }: ChartProps) {
  const [active, setActive] = useState<number | null>(null)
  const { points, bucketMs } = series
  const n = points.length
  const W = 600
  const H = 120
  const bottom = 18
  const max = Math.max(8, ...points.map((p) => p.calls))
  const x = (i: number) => (n > 1 ? (i / (n - 1)) * W : 0)
  const y = (v: number) => H - bottom - (v / max) * (H - bottom - 6)
  const xPct = (i: number) => (x(i) / W) * 100
  const yPct = (v: number) => (y(v) / H) * 100
  const line = points.map((p, i) => `${x(i).toFixed(1)},${y(p.calls).toFixed(1)}`).join(' ')
  const mid = Math.round(max / 2)
  const index = active === null || n === 0 ? null : Math.min(active, n - 1)
  const point = index === null ? null : points[index]
  const ticks = [0.25, 0.5, 0.75].map((f) => Math.round(f * (n - 1)))

  const pick = (e: PointerEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect()
    const fraction = (e.clientX - rect.left) / rect.width
    setActive(Math.max(0, Math.min(n - 1, Math.round(fraction * (n - 1)))))
  }

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const from = index ?? n - 1
    const next: Record<string, number> = { ArrowLeft: from - 1, ArrowRight: from + 1, Home: 0, End: n - 1 }
    if (e.key === 'Escape') {
      setActive(null)
    } else if (e.key in next) {
      e.preventDefault()
      setActive(Math.max(0, Math.min(n - 1, next[e.key])))
    }
  }

  const tip =
    point && index !== null
      ? {
          when: rangeLabel(point, bucketMs, window),
          calls: count(point.calls, 'call'),
          errors: count(point.errors, 'error'),
          side: xPct(index) > 50 ? { right: `calc(${100 - xPct(index)}% + 10px)` } : { left: `calc(${xPct(index)}% + 10px)` },
        }
      : null

  return (
    <div
      className="chart-box"
      tabIndex={0}
      role="group"
      aria-label={`Calls per ${bucket} over the last ${span}. Use the arrow keys to read each bucket.`}
      onPointerDown={pick}
      onPointerMove={pick}
      onPointerLeave={(e) => {
        // A touch keeps its tooltip after the finger lifts. A tap outside the chart removes it.
        if (e.pointerType === 'mouse') setActive(null)
      }}
      onFocus={() => setActive((a) => a ?? n - 1)}
      onBlur={() => setActive(null)}
      onKeyDown={onKey}
    >
      <div className="chart-plot">
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" aria-hidden="true">
          <line className="grid" x1="0" x2={W} y1={y(0)} y2={y(0)} />
          <line className="grid" x1="0" x2={W} y1={y(mid)} y2={y(mid)} strokeDasharray="2 4" />
          <polygon className="area" points={`0,${y(0)} ${line} ${W},${y(0)}`} />
          <polyline className="lineP" points={line} />
          {index !== null && <line className="guide" x1={x(index)} x2={x(index)} y1="0" y2={y(0)} />}
        </svg>
        <span className="chart-label" style={{ top: `${yPct(mid)}%` }}>
          {mid}
        </span>
        {points.map((p, i) =>
          p.errors > 0 ? <span key={p.start} className="chart-dot err" style={{ left: `${xPct(i)}%`, top: `${yPct(p.calls)}%` }} /> : null,
        )}
        {point && index !== null ? (
          <span className="chart-dot active" style={{ left: `${xPct(index)}%`, top: `${yPct(point.calls)}%` }} />
        ) : (
          n > 0 && <span className="chart-dot" style={{ left: `${xPct(n - 1)}%`, top: `${yPct(points[n - 1].calls)}%` }} />
        )}
        {tip && (
          <div className="chart-tip" style={tip.side}>
            <div className="when">{tip.when}</div>
            <div>{tip.calls}</div>
            <div className={point && point.errors > 0 ? 'err' : undefined}>{tip.errors}</div>
          </div>
        )}
      </div>
      <div className="chart-axis" aria-hidden="true">
        <span style={{ left: 0 }}>{span} ago</span>
        {n > 4 &&
          ticks.map((i) => (
            <span key={i} className="mid" style={{ left: `${xPct(i)}%` }}>
              {tickLabel(points[i].start, window)}
            </span>
          ))}
        <span style={{ right: 0 }}>now</span>
      </div>
      <div className="sr-only" aria-live="polite">
        {tip ? `${tip.when}: ${tip.calls}, ${tip.errors}` : ''}
      </div>
    </div>
  )
}
