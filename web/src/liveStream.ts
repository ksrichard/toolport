// Reads GET /api/servers/{name}/live. EventSource cannot send the Bearer header, so this
// module parses the SSE stream from a fetch response.

import { useEffect, useReducer } from 'react'
import { ApiError, openStream } from './api'
import type { Bucket } from './types/Bucket'
import type { CallRecord } from './types/CallRecord'
import type { ConnectedClient } from './types/ConnectedClient'
import type { Metrics } from './types/Metrics'
import type { MetricsUpdate } from './types/MetricsUpdate'
import type { Snapshot } from './types/Snapshot'

export const BUCKET_MS = 5_000
export const CHART_BUCKETS = 60
const MAX_CALLS = 50
const RETRY_MS = 3_000

export type StreamStatus = 'connecting' | 'live' | 'paused' | 'reconnecting' | 'failed'

export interface LiveState {
  status: StreamStatus
  error: string | null
  metrics: Metrics | null
  clients: ConnectedClient[]
  /** Calls and errors per bucket start time. */
  buckets: Map<number, { calls: number; errors: number }>
  /** The newest call first. */
  calls: CallRecord[]
  /** Ids of calls that came after the snapshot. The call log highlights them. */
  fresh: Set<number>
}

type Action =
  | { type: 'status'; status: StreamStatus; error?: string }
  | { type: 'snapshot'; data: Snapshot }
  | { type: 'call'; data: CallRecord }
  | { type: 'metrics'; data: MetricsUpdate }

const initial: LiveState = {
  status: 'connecting',
  error: null,
  metrics: null,
  clients: [],
  buckets: new Map(),
  calls: [],
  fresh: new Set(),
}

function bucketsFrom(list: Bucket[]): LiveState['buckets'] {
  return new Map(list.map((b) => [b.start_ms, { calls: b.calls, errors: b.errors }]))
}

function reduce(state: LiveState, action: Action): LiveState {
  switch (action.type) {
    case 'status':
      return { ...state, status: action.status, error: action.error ?? null }
    case 'snapshot':
      return {
        ...state,
        status: 'live',
        error: null,
        metrics: action.data.metrics,
        clients: action.data.clients,
        buckets: bucketsFrom(action.data.buckets),
        calls: action.data.calls.slice(0, MAX_CALLS),
        fresh: new Set(),
      }
    case 'call': {
      const call = action.data
      // The snapshot query and the call event can both contain the same call.
      if (state.calls.some((c) => c.id === call.id)) return state
      const start = Math.floor(call.at_ms / BUCKET_MS) * BUCKET_MS
      const buckets = new Map(state.buckets)
      const bucket = buckets.get(start) ?? { calls: 0, errors: 0 }
      buckets.set(start, {
        calls: bucket.calls + 1,
        errors: bucket.errors + (call.result === 'ok' ? 0 : 1),
      })
      return {
        ...state,
        buckets,
        calls: [call, ...state.calls].slice(0, MAX_CALLS),
        fresh: new Set(state.fresh).add(call.id),
      }
    }
    case 'metrics':
      return { ...state, metrics: action.data.metrics, clients: action.data.clients }
  }
}

/** Splits an SSE byte stream into events. It ignores comment lines, which are the keep-alives. */
async function* readEvents(body: ReadableStream<Uint8Array>) {
  const reader = body.pipeThrough<string>(new TextDecoderStream() as ReadableWritablePair<string, Uint8Array>).getReader()
  let buffer = ''
  for (;;) {
    const { value, done } = await reader.read()
    if (done) return
    buffer += value.replace(/\r\n/g, '\n')
    let end
    while ((end = buffer.indexOf('\n\n')) >= 0) {
      const block = buffer.slice(0, end)
      buffer = buffer.slice(end + 2)
      let event = 'message'
      const data: string[] = []
      for (const line of block.split('\n')) {
        if (line.startsWith('event:')) event = line.slice(6).trim()
        else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''))
      }
      if (data.length > 0) yield { event, data: data.join('\n') }
    }
  }
}

/** Keeps the live stream of one server open. Pause closes the stream. Resume opens it again
 * and gets a new snapshot, so no call is missing after a pause. */
export function useLiveStream(server: string, paused: boolean): LiveState {
  const [state, dispatch] = useReducer(reduce, initial)

  useEffect(() => {
    if (paused) {
      dispatch({ type: 'status', status: 'paused' })
      return
    }
    const abort = new AbortController()
    let retry: ReturnType<typeof setTimeout> | undefined

    const connect = async (attempt: number) => {
      dispatch({ type: 'status', status: attempt === 0 ? 'connecting' : 'reconnecting' })
      try {
        const res = await openStream(`/api/servers/${encodeURIComponent(server)}/live`, abort.signal)
        if (!res.body) throw new Error('the browser cannot read the stream')
        for await (const { event, data } of readEvents(res.body)) {
          const parsed = JSON.parse(data)
          if (event === 'snapshot') dispatch({ type: 'snapshot', data: parsed })
          else if (event === 'call') dispatch({ type: 'call', data: parsed })
          else if (event === 'metrics') dispatch({ type: 'metrics', data: parsed })
        }
        throw new Error('the server closed the stream')
      } catch (error) {
        if (abort.signal.aborted) return
        // A 4xx does not get better with a retry.
        if (error instanceof ApiError && error.status < 500) {
          dispatch({ type: 'status', status: 'failed', error: error.message })
          return
        }
        dispatch({ type: 'status', status: 'reconnecting' })
        retry = setTimeout(() => connect(attempt + 1), RETRY_MS)
      }
    }
    connect(0)
    return () => {
      abort.abort()
      clearTimeout(retry)
    }
  }, [server, paused])

  return state
}
