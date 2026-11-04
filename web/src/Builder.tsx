// Tool builder. The left side edits a tool. The right side shows the JSON that Claude gets
// from tools/list. The Test section runs the draft and shows the MCP result and the raw HTTP.

import { useEffect, useRef, useState, type ReactNode } from 'react'
import {
  ApiError,
  createManualApi,
  createServerWithTool,
  createTool,
  deleteTool,
  detachTool,
  getApis,
  getTool,
  resetTool,
  testDraftTool,
  testTool,
  updateTool,
} from './api'
import { authInput, authOf, isApiDirty, saveApiEdit, type AuthState } from './apiAuth'
import { AuthFields } from './AuthFields'
import { parseQueryTemplate, pathPlaceholders, schemaFromRows, toolJson } from './inputs'
import { useListAnimation } from './motion'
import type { ApiView } from './types/ApiView'
import type { HeaderRow } from './types/HeaderRow'
import type { HttpMethod } from './types/HttpMethod'
import type { InputRow } from './types/InputRow'
import type { InputType } from './types/InputType'
import type { ServerSummary } from './types/ServerSummary'
import type { TestApi } from './types/TestApi'
import type { TestResult } from './types/TestResult'
import type { ToolDraft } from './types/ToolDraft'
import type { ToolView } from './types/ToolView'

type Schema = Record<string, unknown>

const METHODS: HttpMethod[] = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS', 'TRACE']
/** The same rules as `valid_input_name` and `is_secret_header` in `crates/server/src/tools.rs`. */
const validInputName = (name: string) => /^[A-Za-z_][A-Za-z0-9_-]{0,63}$/.test(name) && name !== '__proto__'
const isSecretHeader = (name: string) => /key|token|secret|auth|cookie/i.test(name.trim())
const inputNameMessage = (name: string) =>
  `The input name "${name}" must start with a letter or _ and have 1 to 64 letters, digits, _ or -.`
const SECRET_HEADER_MESSAGE = 'Put credentials in the API auth, not in a fixed header.'
const isInputValue = (value: string) => /^\{[^{}]*\}$/.test(value)

const INPUT_TYPES: InputType[] = ['string', 'number', 'integer', 'boolean', 'string_array', 'complex']

export type Saved = { kind: 'tool'; tool: ToolView; reset?: boolean } | { kind: 'server'; server: ServerSummary }

const message = (error: unknown) => (error instanceof ApiError || error instanceof Error ? error.message : String(error))

/** For a spec tool the preview starts from the stored schema and applies only the edited
 * description and required flags. The backend also applies type changes, but this form
 * locks the type of spec rows, so the result is the same. */
function specSchema(preview: unknown, rows: InputRow[]): Schema {
  const stored = ((preview as { inputSchema?: Schema } | null)?.inputSchema ?? { type: 'object', properties: {} }) as Schema
  const properties: Schema = { ...((stored.properties as Schema | undefined) ?? {}) }
  for (const row of rows) {
    const property = properties[row.name]
    if (typeof property !== 'object' || property === null) continue
    const next = { ...(property as Schema) }
    if (row.description === '') delete next.description
    else next.description = row.description
    properties[row.name] = next
  }
  const old = Array.isArray(stored.required) ? (stored.required as string[]) : []
  const required = [...old.filter((name) => !rows.some((r) => r.name === name)), ...rows.filter((r) => r.required).map((r) => r.name)]
  const schema: Schema = { ...stored, properties }
  if (required.length > 0) schema.required = required
  else delete schema.required
  return schema
}

/** A tool detached from a spec can keep complex inputs. The server keeps their stored
 * property and applies only the description, so the preview does the same. */
function keepComplex(schema: Schema, preview: unknown, rows: InputRow[]): Schema {
  const stored = (preview as { inputSchema?: { properties?: Schema } } | null)?.inputSchema?.properties ?? {}
  const properties: Schema = { ...(schema.properties as Schema) }
  for (const row of rows) {
    const property = stored[row.name]
    if (row.input_type !== 'complex' || property === undefined) continue
    if (typeof property !== 'object' || property === null || Array.isArray(property)) {
      properties[row.name] = property
      continue
    }
    const next = { ...(property as Schema) }
    if (row.description === '') delete next.description
    else next.description = row.description
    properties[row.name] = next
  }
  return { ...schema, properties }
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys)
  if (typeof value !== 'object' || value === null) return value
  return Object.fromEntries(
    Object.entries(value)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([k, v]) => [k, sortKeys(v)]),
  )
}

// ---- colored JSON ----

type Token = [cls: string, text: string]

const isPrimitive = (v: unknown) => v === null || typeof v !== 'object'

function primitive(v: unknown): Token {
  if (typeof v === 'string') return ['j-str', JSON.stringify(v)]
  if (typeof v === 'number') return ['j-num', String(v)]
  return ['j-lit', String(v)]
}

/** Pretty-prints like the slide: small objects and arrays of simple values stay on one line. */
function tokens(value: unknown, depth: number, out: Token[]) {
  const pad = '  '.repeat(depth + 1)
  const end = '  '.repeat(depth)
  if (isPrimitive(value)) {
    out.push(primitive(value))
  } else if (Array.isArray(value)) {
    if (value.every(isPrimitive)) {
      out.push(['', '['])
      value.forEach((item, i) => {
        if (i > 0) out.push(['', ', '])
        out.push(primitive(item))
      })
      out.push(['', ']'])
    } else {
      out.push(['', '[\n'])
      value.forEach((item, i) => {
        out.push(['', pad])
        tokens(item, depth + 1, out)
        out.push(['', i < value.length - 1 ? ',\n' : '\n'])
      })
      out.push(['', `${end}]`])
    }
  } else {
    const entries = Object.entries(value as Schema)
    if (entries.length === 0) {
      out.push(['', '{}'])
    } else if (entries.every(([, v]) => isPrimitive(v)) && entries.length <= 3) {
      out.push(['', '{ '])
      entries.forEach(([k, v], i) => {
        if (i > 0) out.push(['', ', '])
        out.push(['j-key', JSON.stringify(k)], ['', ': '], primitive(v))
      })
      out.push(['', ' }'])
    } else {
      out.push(['', '{\n'])
      entries.forEach(([k, v], i) => {
        out.push(['', pad], ['j-key', JSON.stringify(k)], ['', ': '])
        tokens(v, depth + 1, out)
        out.push(['', i < entries.length - 1 ? ',\n' : '\n'])
      })
      out.push(['', `${end}}`])
    }
  }
}

function Json({ value }: { value: unknown }) {
  const out: Token[] = []
  tokens(value, 0, out)
  return (
    <pre className="code">
      {out.map(([cls, text], i) => (cls ? <span key={i} className={cls}>{text}</span> : text))}
    </pre>
  )
}

// ---- the form ----

interface ToolFormProps {
  /** The server that owns the tool. Null in onboarding, where the server does not exist yet. */
  server: string | null
  /** The tool to edit. Null for a new tool. */
  initial: ToolView | null
  onSaved: (result: Saved) => void
  onDeleted?: () => void
  /** Onboarding: show a server name field. Save creates the server with this tool. */
  onboarding?: boolean
}

export function ToolForm({ server, initial, onSaved, onDeleted, onboarding = false }: ToolFormProps) {
  const isSpec = initial?.source === 'spec'
  const draft0 = initial?.draft
  const [serverName, setServerName] = useState('')
  const [name, setName] = useState(draft0?.name ?? '')
  const [about, setAbout] = useState(draft0?.description ?? '')
  const [method, setMethod] = useState<HttpMethod>(draft0?.method ?? 'GET')
  const [path, setPath] = useState(draft0?.path ?? '')
  const [query, setQuery] = useState(draft0?.query ?? '')
  const [headers, setHeaders] = useState<HeaderRow[]>(draft0?.headers ?? [])
  const [bodyText, setBodyText] = useState(
    draft0?.body_schema === undefined || draft0.body_schema === null ? '' : JSON.stringify(draft0.body_schema, null, 2),
  )
  const [rows, setRows] = useState<InputRow[]>(draft0?.inputs ?? [])
  const headerKeys = useRowKeys(headers.length)
  const rowKeys = useRowKeys(rows.length)
  const headersRef = useListAnimation<HTMLDivElement>()
  const rowsRef = useListAnimation<HTMLTableSectionElement>()

  const [apis, setApis] = useState<ApiView[] | null>(server === null ? [] : null)
  const [apiId, setApiId] = useState<number | null>(initial?.api_id ?? null)
  const [baseUrl, setBaseUrl] = useState('')
  const [auth, setAuth] = useState<AuthState>(authOf(undefined))

  const [samples, setSamples] = useState<Record<string, string>>({})
  const [result, setResult] = useState<TestResult | null>(null)
  // Each test run remounts the result, so a new result fades in also when it looks the same.
  const [testRuns, setTestRuns] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [status, setStatus] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const initialApiId = initial?.api_id
  const selectApi = (list: ApiView[], id: number | null) => {
    const api = list.find((a) => a.id === id)
    setApiId(api ? api.id : null)
    setBaseUrl(api?.base_url ?? '')
    setAuth(authOf(api))
  }

  useEffect(() => {
    if (server === null) return
    let cancelled = false
    getApis(server).then(
      (list) => {
        if (cancelled) return
        setApis(list)
        selectApi(list, initialApiId ?? list[0]?.id ?? null)
      },
      (e: unknown) => !cancelled && setError(message(e)),
    )
    return () => {
      cancelled = true
    }
  }, [server, initialApiId])

  const api = apis?.find((a) => a.id === apiId)
  const apiDirty = api !== undefined && isApiDirty(api, baseUrl, auth)

  // ---- derived values ----
  const filledRows = rows.filter((r) => r.name.trim() !== '').map((r) => ({ ...r, name: r.name.trim() }))
  let body: unknown
  let bodyError: string | null = null
  if (bodyText.trim() !== '') {
    try {
      body = JSON.parse(bodyText)
    } catch {
      bodyError = 'The body schema is not valid JSON.'
    }
  }
  const queryCheck = parseQueryTemplate(query)
  const queryError = queryCheck instanceof Error ? queryCheck.message : null
  const badName = isSpec ? undefined : filledRows.find((r) => !validInputName(r.name))
  const secretHeader = isSpec ? undefined : headers.find((h) => isSecretHeader(h.name) && !isInputValue(h.value))
  const pathNames = pathPlaceholders(path)
  const placeholders = pathPlaceholders(path).filter((p) => !filledRows.some((r) => r.name === p))

  let schema: Schema
  if (isSpec && initial) schema = specSchema(initial.preview, filledRows)
  else if (initial) schema = keepComplex(schemaFromRows(filledRows, body), initial.preview, filledRows)
  else schema = schemaFromRows(filledRows, body)
  // The server sends the keys of inputSchema in sorted order, so the preview sorts them too.
  const preview = toolJson(name.trim(), about, sortKeys(schema) as Schema)

  const buildDraft = (): ToolDraft => {
    const draft: ToolDraft = {
      name: name.trim(),
      description: about,
      method,
      path,
      query,
      headers: headers.filter((h) => h.name.trim() !== ''),
      inputs: filledRows,
    }
    if (body !== undefined) draft.body_schema = body
    return draft
  }

  /** The host of the base URL. A number is added when an API already has that name. */
  const apiNameFor = () => {
    let host = baseUrl.trim()
    try {
      host = new URL(host).hostname
    } catch {
      // Keep the text. The server rejects a bad URL.
    }
    const taken = new Set((apis ?? []).map((a) => a.name))
    let candidate = host
    for (let n = 2; taken.has(candidate); n++) candidate = `${host}-${n}`
    return candidate
  }
  const newApi = () => ({ name: apiNameFor(), base_url: baseUrl.trim(), auth: authInput(auth) })

  const run = async (action: () => Promise<void>) => {
    setBusy(true)
    setError(null)
    setStatus(null)
    try {
      await action()
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }

  /** Makes sure the API exists and is up to date. Returns its id. */
  const saveApi = async (srv: string): Promise<number> => {
    if (apiId === null) {
      const created = await createManualApi(srv, newApi())
      const list = [...(apis ?? []), created]
      setApis(list)
      selectApi(list, created.id)
      return created.id
    }
    if (api && apiDirty) {
      const updated = await saveApiEdit(srv, api, baseUrl, auth)
      const list = (apis ?? []).map((a) => (a.id === updated.id ? updated : a))
      setApis(list)
      selectApi(list, updated.id)
    }
    return apiId
  }

  const save = () =>
    run(async () => {
      if (bodyError || queryError) throw new Error(bodyError ?? queryError ?? '')
      if (badName) throw new Error(inputNameMessage(badName.name))
      if (secretHeader) throw new Error(SECRET_HEADER_MESSAGE)
      if (onboarding || server === null) {
        const created = await createServerWithTool({ server: serverName.trim(), api: newApi(), draft: buildDraft() })
        onSaved({ kind: 'server', server: created })
        return
      }
      const id = await saveApi(server)
      const draft = buildDraft()
      const tool = initial
        ? await updateTool(server, initial.draft.name, { api_id: id, draft })
        : await createTool(server, { api_id: id, draft })
      setStatus('Saved.')
      onSaved({ kind: 'tool', tool })
    })

  const reset = () =>
    run(async () => {
      if (server === null || !initial) return
      onSaved({ kind: 'tool', tool: await resetTool(server, initial.draft.name), reset: true })
    })

  const detach = () =>
    run(async () => {
      if (server === null || !initial) return
      if (!window.confirm('Detach this tool from the spec? A re-import will no longer update it. This cannot be undone.')) return
      onSaved({ kind: 'tool', tool: await detachTool(server, initial.draft.name), reset: true })
    })

  const remove = () =>
    run(async () => {
      if (server === null || !initial) return
      if (!window.confirm(`Delete the tool ${initial.draft.name}?`)) return
      await deleteTool(server, initial.draft.name)
      onDeleted?.()
    })

  const test = () =>
    run(async () => {
      const args: Record<string, unknown> = {}
      for (const row of filledRows) {
        const text = (samples[row.name] ?? '').trim()
        if (text === '') continue
        args[row.name] = sampleValue(row, text)
      }
      const bodySample = (samples.body ?? '').trim()
      if (body !== undefined && bodySample !== '') args.body = sampleValue({ input_type: 'complex' }, bodySample)
      let testApi: TestApi
      if (apiId === null) testApi = { kind: 'draft', base_url: baseUrl.trim(), auth: authInput(auth) }
      else testApi = { kind: 'saved', id: apiId, auth: apiDirty ? authInput(auth) : undefined }
      const stored = initial ? initial.draft.name : undefined
      const request = {
        draft: buildDraft(),
        api: testApi,
        args,
        spec_tool: isSpec ? stored : undefined,
        manual_tool: isSpec ? undefined : stored,
      }
      setResult(server === null ? await testDraftTool(request) : await testTool(server, request))
      setTestRuns((n) => n + 1)
    })

  // ---- row helpers ----
  const patchRow = (i: number, patch: Partial<InputRow>) => setRows(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)))
  const patchHeader = (i: number, patch: Partial<HeaderRow>) => setHeaders(headers.map((h, j) => (j === i ? { ...h, ...patch } : h)))

  return (
    <div className="builder">
      {error && (
        <p className="alert" role="alert">
          {error}
        </p>
      )}
      <div className="builder-grid">
        <section className="panel form">
          <h2>
            {initial ? 'Edit tool' : 'New tool'}
            <button className="btn primary" type="button" onClick={test} disabled={busy}>
              ▷ Test
            </button>
          </h2>

          {isSpec && (
            <div className="spec-note">
              <span className="muted">These fields come from the spec, so a re-import can update them.</span>
              <button className="btn" type="button" onClick={detach} disabled={busy}>
                Detach from spec
              </button>
            </div>
          )}
          {onboarding && (
            <Row label="server">
              <input value={serverName} onChange={(e) => setServerName(e.target.value)} placeholder="my-server" aria-label="Server name" />
            </Row>
          )}
          <Row label="name">
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="get_weather" aria-label="Tool name" />
          </Row>
          <Row label="about">
            <input value={about} onChange={(e) => setAbout(e.target.value)} placeholder="Current weather at a place" aria-label="Tool description" />
          </Row>

          <Row label="url">
            <div className="line">
              <select value={method} onChange={(e) => setMethod(e.target.value as HttpMethod)} disabled={isSpec} aria-label="HTTP method">
                {METHODS.map((m) => (
                  <option key={m}>{m}</option>
                ))}
              </select>
              <select
                value={apiId ?? 'new'}
                disabled={isSpec || apis === null}
                aria-label="API"
                onChange={(e) => selectApi(apis ?? [], e.target.value === 'new' ? null : Number(e.target.value))}
              >
                {apis?.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name}
                  </option>
                ))}
                <option value="new">New API…</option>
              </select>
            </div>
            <div className="line">
              <input
                className="grow"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="https://api.open-meteo.com"
                aria-label="API base URL"
              />
              <input className="grow" value={path} onChange={(e) => setPath(e.target.value)} readOnly={isSpec} placeholder="/v1/forecast" aria-label="Path" />
            </div>
            {placeholders.length > 0 && <small className="bad">Add an input for {placeholders.map((p) => `{${p}}`).join(', ')}.</small>}
          </Row>

          <Row label="query">
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              readOnly={isSpec}
              placeholder="latitude={lat}&longitude={lon}"
              aria-label="Query template"
              aria-invalid={queryError !== null}
            />
            {queryError && <small className="bad">{queryError}</small>}
          </Row>

          <Row label="headers">
            <div className="stack" ref={headersRef}>
              {headers.map((h, i) => (
                <div className="line" key={headerKeys.keys[i]}>
                  <input className="grow" value={h.name} onChange={(e) => patchHeader(i, { name: e.target.value })} readOnly={isSpec} placeholder="Accept" aria-label="Header name" />
                  <input className="grow" value={h.value} onChange={(e) => patchHeader(i, { value: e.target.value })} readOnly={isSpec} placeholder="application/json or {input}" aria-label="Header value" />
                  {!isSpec && (
                    <button
                      className="btn"
                      type="button"
                      onClick={() => {
                        setHeaders(headers.filter((_, j) => j !== i))
                        headerKeys.remove(i)
                      }}
                      aria-label="Remove header"
                    >
                      ×
                    </button>
                  )}
                </div>
              ))}
            </div>
            {!isSpec && (
              <button
                className="btn add"
                type="button"
                onClick={() => {
                  setHeaders([...headers, { name: '', value: '' }])
                  headerKeys.add()
                }}
              >
                Add header
              </button>
            )}
            {isSpec && headers.length === 0 && <span className="muted">none</span>}
            {secretHeader && <small className="bad">{SECRET_HEADER_MESSAGE}</small>}
          </Row>

          <Row label="body">
            <textarea
              value={bodyText}
              onChange={(e) => setBodyText(e.target.value)}
              readOnly={isSpec}
              rows={bodyText === '' ? 2 : 6}
              placeholder="Optional. A JSON schema for the request body."
              aria-label="Body schema"
              aria-invalid={bodyError !== null}
            />
            {bodyError && <small className="bad">{bodyError}</small>}
          </Row>

          <Row label="auth">
            <AuthFields auth={auth} onChange={setAuth} api={api} />
            <small>This auth applies to every tool of this API.</small>
          </Row>

          <Row label="inputs">
            <div className="scroll">
              <table className="inputs">
                <thead>
                  <tr>
                    <th>name</th>
                    <th>type</th>
                    <th>required</th>
                    <th>description</th>
                    <th />
                  </tr>
                </thead>
                <tbody ref={rowsRef}>
                  {rows.map((r, i) => {
                    const complex = r.input_type === 'complex'
                    const fixed = isSpec && complex
                    const inPath = isSpec && pathNames.includes(r.name)
                    return (
                      <tr key={rowKeys.keys[i]}>
                        <td>
                          <input value={r.name} onChange={(e) => patchRow(i, { name: e.target.value })} readOnly={isSpec || complex} aria-label="Input name" />
                        </td>
                        <td>
                          <select value={r.input_type} onChange={(e) => patchRow(i, { input_type: e.target.value as InputType })} disabled={isSpec || complex} aria-label="Input type">
                            {INPUT_TYPES.filter((t) => t !== 'complex' || r.input_type === 'complex').map((t) => (
                              <option key={t}>{t}</option>
                            ))}
                          </select>
                        </td>
                        <td>
                          <input type="checkbox" checked={r.required} onChange={(e) => patchRow(i, { required: e.target.checked })} disabled={fixed || inPath} aria-label="Required" />
                        </td>
                        <td>
                          <input value={r.description} onChange={(e) => patchRow(i, { description: e.target.value })} readOnly={fixed} aria-label="Input description" />
                        </td>
                        <td>
                          {!isSpec && (
                            <button
                              className="btn"
                              type="button"
                              onClick={() => {
                                setRows(rows.filter((_, j) => j !== i))
                                rowKeys.remove(i)
                              }}
                              aria-label="Remove input"
                            >
                              ×
                            </button>
                          )}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
            {badName && <small className="bad">{inputNameMessage(badName.name)}</small>}
            {!isSpec && (
              <button
                className="btn add"
                type="button"
                onClick={() => {
                  setRows([...rows, { name: '', input_type: 'string', required: true, description: '' }])
                  rowKeys.add()
                }}
              >
                Add input
              </button>
            )}
          </Row>

          {initial?.spec_changed && <p className="alert">The spec changed this tool after your edit.</p>}
          <div className="actions">
            {isSpec && (
              <button className="btn" type="button" onClick={reset} disabled={busy || !initial?.edited}>
                Reset to spec
              </button>
            )}
            {initial && !isSpec && (
              <button className="btn danger" type="button" onClick={remove} disabled={busy}>
                Delete
              </button>
            )}
            {status && <span className="muted">{status}</span>}
            <button className="btn primary" type="button" onClick={save} disabled={busy}>
              Save
            </button>
          </div>
        </section>

        <div className="arrow" aria-hidden="true">
          →
        </div>

        <section className="panel see">
          <h2>WHAT CLAUDE SEES</h2>
          <Json value={preview} />
        </section>
      </div>

      <section className="panel">
        <h2>
          Test <small>sample values for the inputs</small>
        </h2>
        {filledRows.length === 0 && body === undefined && <p className="muted">This tool has no inputs.</p>}
        <div className="samples">
          {filledRows.map((r) => (
            <label className="field" key={r.name}>
              <span>
                {r.name} <small>{r.input_type === 'string_array' ? 'comma separated' : r.input_type}</small>
              </span>
              <input value={samples[r.name] ?? ''} onChange={(e) => setSamples({ ...samples, [r.name]: e.target.value })} />
            </label>
          ))}
          {body !== undefined && (
            <label className="field">
              <span>
                body <small>JSON</small>
              </span>
              <input value={samples.body ?? ''} onChange={(e) => setSamples({ ...samples, body: e.target.value })} />
            </label>
          )}
        </div>
      </section>

      {result && (
        <div className="builder-grid results" key={testRuns}>
          <section className="panel">
            <h2>MCP result</h2>
            <Json value={result.mcp} />
          </section>
          <div className="arrow" aria-hidden="true" />
          <section className="panel">
            <h2>
              Raw HTTP <small>{result.http.latency_ms} ms</small>
            </h2>
            <HttpView trace={result.http} />
          </section>
        </div>
      )}
    </div>
  )
}

/**
 * Stable React keys for rows that the user adds and removes. Index keys would make the list
 * animation fade out the last row when the user removes a row in the middle.
 */
function useRowKeys(initial: number) {
  const next = useRef(initial)
  const [keys, setKeys] = useState(() => Array.from({ length: initial }, (_, i) => i))
  return {
    keys,
    add: () => {
      const key = next.current++
      setKeys((list) => [...list, key])
    },
    remove: (index: number) => setKeys((list) => list.filter((_, i) => i !== index)),
  }
}

function sampleValue(row: Pick<InputRow, 'input_type'>, text: string): unknown {
  switch (row.input_type) {
    case 'number':
    case 'integer':
      return Number.isNaN(Number(text)) ? text : Number(text)
    case 'boolean':
      return text === 'true'
    case 'string_array':
      return text.split(',').map((s) => s.trim())
    case 'complex':
      try {
        return JSON.parse(text)
      } catch {
        return text
      }
    default:
      return text
  }
}

export function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="row">
      <div className="lbl">{label}</div>
      <div className="ctl">{children}</div>
    </div>
  )
}

function HttpView({ trace }: { trace: TestResult['http'] }) {
  const { request, response } = trace
  const head = (pairs: [string, string][]) => pairs.map(([k, v]) => `${k}: ${v}`).join('\n')
  return (
    <>
      <pre className="code">
        <span className="j-key">{request.method}</span> {request.url}
        {'\n'}
        {head(request.headers)}
        {request.body !== null && `\n\n${request.body}`}
      </pre>
      {response ? (
        <pre className="code">
          <span className={response.status < 400 ? 'j-str' : 'j-bad'}>HTTP {response.status}</span>
          {'\n'}
          {head(response.headers)}
          {`\n\n${response.body}`}
          {response.cut && '\n… body cut'}
        </pre>
      ) : (
        <p className="muted">No response.</p>
      )}
    </>
  )
}

// ---- the page ----

interface BuilderProps {
  server: string
  /** The tool name from the route. Null for a new tool. */
  tool: string | null
  navigate: (path: string) => void
  onChanged: () => void
}

export function Builder({ server, tool, navigate, onChanged }: BuilderProps) {
  const [view, setView] = useState<ToolView | null>(null)
  const [loaded, setLoaded] = useState(tool === null)
  const [error, setError] = useState<string | null>(null)
  const [version, setVersion] = useState(0)
  const toolsPath = `/servers/${encodeURIComponent(server)}/tools`

  useEffect(() => {
    if (tool === null) return
    let cancelled = false
    getTool(server, tool).then(
      (v) => {
        if (cancelled) return
        setView(v)
        setLoaded(true)
      },
      (e: unknown) => !cancelled && setError(message(e)),
    )
    return () => {
      cancelled = true
    }
  }, [server, tool])

  const [back] = useState(() => (window.history.state?.from === '/tools' ? '/tools' : '/'))

  const saved = (result: Saved) => {
    if (result.kind !== 'tool') return
    onChanged()
    setView(result.tool)
    if (result.reset) setVersion((v) => v + 1)
    if (result.tool.draft.name !== tool) navigate(`${toolsPath}/${encodeURIComponent(result.tool.draft.name)}`)
  }

  return (
    <>
      <div className="head">
        <div>
          <h1>{tool ?? 'New tool'}</h1>
          <div className="sub">
            <a
              href={back}
              onClick={(e) => {
                e.preventDefault()
                navigate(back)
              }}
            >
              {back === '/tools' ? 'Back to Tools' : `Back to ${server}`}
            </a>
          </div>
        </div>
      </div>
      {error ? (
        <p className="alert" role="alert">
          Could not load the tool: {error}
        </p>
      ) : loaded ? (
        <ToolForm
          key={`${tool ?? 'new'}/${version}`}
          server={server}
          initial={view}
          onSaved={saved}
          onDeleted={() => {
            onChanged()
            navigate(back)
          }}
        />
      ) : null}
    </>
  )
}
