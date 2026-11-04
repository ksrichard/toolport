// APIs page: the APIs of the server with their base URL, auth, and spec source.
// A spec API can be imported again, from its URL or from a new file. Any API can be removed
// with its tools, while one enabled tool stays on the server.

import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { ApiError, getApis, getTemplates, importApi, reimportApi, removeApi, setAutoCheck } from './api'
import { authOf, isApiDirty, saveApiEdit, type AuthState } from './apiAuth'
import { AuthFields } from './AuthFields'
import { Row } from './Builder'
import { readSpecFile, useSpecSource } from './specSource'
import { SpecSourceFields } from './SpecSourceFields'
import type { ApiView } from './types/ApiView'
import type { ImportedApi } from './types/ImportedApi'
import type { Template } from './types/Template'

const message = (error: unknown) => (error instanceof ApiError || error instanceof Error ? error.message : String(error))

interface ApisProps {
  server: string
  /** Reloads the server list, because the counts in the side menu change. */
  onChanged: () => void
}

export function Apis({ server, onChanged }: ApisProps) {
  const [apis, setApis] = useState<ApiView[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)
  const [report, setReport] = useState<{ title: string; apis: ImportedApi[] } | null>(null)

  const load = useCallback(() => {
    getApis(server).then(
      (list) => {
        setApis(list)
        setLoadError(null)
      },
      (e: unknown) => setLoadError(message(e)),
    )
  }, [server])
  useEffect(load, [load])

  const imported = (title: string, list: ImportedApi[]) => {
    setReport({ title, apis: list })
    load()
    onChanged()
  }

  return (
    <div className="builder">
      <div className="head">
        <div>
          <h1>APIs</h1>
          <div className="sub">Each API has one base URL and one auth for all its tools.</div>
        </div>
        <div className="head-actions">
          {!adding && (
            <button className="btn primary" type="button" onClick={() => setAdding(true)}>
              Add API
            </button>
          )}
        </div>
      </div>

      {loadError && (
        <p className="alert" role="alert">
          Could not load the APIs: {loadError}
        </p>
      )}
      {adding && (
        <AddApi
          server={server}
          onCancel={() => setAdding(false)}
          onDone={(list) => {
            setAdding(false)
            imported('Import', list)
          }}
        />
      )}
      {report && <ImportReport title={report.title} apis={report.apis} onClose={() => setReport(null)} />}
      {apis?.length === 0 && <p className="muted">This server has no APIs yet.</p>}
      {apis?.map((api) => (
        <ApiCard
          key={api.id}
          server={server}
          api={api}
          onSaved={(saved) => setApis((list) => list?.map((a) => (a.id === saved.id ? saved : a)) ?? null)}
          onReimported={(result) => imported('Re-import', [result])}
          onRemoved={() => {
            setReport(null)
            load()
            onChanged()
          }}
        />
      ))}
    </div>
  )
}

interface ApiCardProps {
  server: string
  api: ApiView
  onSaved: (api: ApiView) => void
  onReimported: (result: ImportedApi) => void
  onRemoved: () => void
}

function ApiCard({ server, api, onSaved, onReimported, onRemoved }: ApiCardProps) {
  const [baseUrl, setBaseUrl] = useState(api.base_url)
  const [auth, setAuth] = useState<AuthState>(authOf(api))
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [status, setStatus] = useState<string | null>(null)
  const [confirmRemove, setConfirmRemove] = useState(false)
  const isSpec = api.kind === 'spec'
  const dirty = isApiDirty(api, baseUrl, auth)

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

  const save = () =>
    run(async () => {
      const saved = await saveApiEdit(server, api, baseUrl, auth)
      setBaseUrl(saved.base_url)
      setAuth(authOf(saved))
      onSaved(saved)
      setStatus('Saved.')
    })

  const reimport = (spec?: string) =>
    run(async () => {
      onReimported(await reimportApi(server, api.id, { spec }))
    })

  const toggleCheck = (enabled: boolean) =>
    run(async () => {
      onSaved(await setAutoCheck(server, api.id, enabled))
    })

  const remove = () =>
    run(async () => {
      try {
        await removeApi(server, api.id)
      } finally {
        setConfirmRemove(false)
      }
      onRemoved()
    })

  const toolCount = `${api.tool_count} ${api.tool_count === 1 ? 'tool' : 'tools'}`

  return (
    <section className="panel form">
      <h2>
        <span className="tool-name">
          <span>{api.name}</span>
          <span className="method">{isSpec ? 'spec' : 'hand-made'}</span>
        </span>
        <small>{toolCount}</small>
      </h2>
      {error && (
        <p className="alert" role="alert">
          {error}
        </p>
      )}
      <Row label="base url">
        <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://api.example.com" aria-label="API base URL" />
      </Row>
      <Row label="auth">
        <AuthFields auth={auth} onChange={setAuth} api={api} />
        <small>This auth applies to every tool of this API.</small>
      </Row>
      {isSpec && (
        <Row label="spec">
          {api.spec_url ? <span className="mono api-url">{api.spec_url}</span> : <span className="muted">From a file. Pick the new file to import it again.</span>}
        </Row>
      )}
      {api.spec_url && (
        <Row label="check">
          <div className="line">
            <label className="toggle">
              <input
                type="checkbox"
                role="switch"
                checked={api.auto_check}
                disabled={busy}
                onChange={(e) => toggleCheck(e.target.checked)}
                aria-label="Daily check"
              />
              <span />
            </label>
            <span>Daily check</span>
          </div>
          <small>
            {api.last_check_ms === null ? (
              'Not checked yet.'
            ) : (
              <>
                Last check {new Date(api.last_check_ms).toLocaleString()} <CheckResult result={api.last_check_result} />
              </>
            )}
          </small>
        </Row>
      )}
      {confirmRemove && (
        <div className="actions">
          <span className="muted">
            Remove {api.name} and its {toolCount}? Open MCP sessions lose these tools. The call log stays.
          </span>
          <button className="btn" type="button" onClick={() => setConfirmRemove(false)} disabled={busy}>
            Cancel
          </button>
          <button className="btn danger" type="button" onClick={remove} disabled={busy}>
            {busy ? 'Removing…' : 'Yes, remove'}
          </button>
        </div>
      )}
      <div className="actions">
        {status && <span className="muted">{status}</span>}
        {!confirmRemove && (
          <button className="btn" type="button" onClick={() => setConfirmRemove(true)} disabled={busy} aria-label={`Remove ${api.name}`}>
            Remove
          </button>
        )}
        {isSpec &&
          (api.spec_url ? (
            <button className="btn" type="button" onClick={() => reimport()} disabled={busy}>
              Re-import
            </button>
          ) : (
            <label className={`btn file-btn${busy ? ' disabled' : ''}`}>
              Re-import from file
              <input
                type="file"
                accept=".json,application/json"
                disabled={busy}
                onChange={async (e) => {
                  const file = await readSpecFile(e.target)
                  e.target.value = ''
                  if (file) reimport(file.text)
                }}
              />
            </label>
          ))}
        <button className="btn primary" type="button" onClick={save} disabled={busy || !dirty}>
          Save
        </button>
      </div>
    </section>
  )
}

function CheckResult({ result }: { result: string | null }) {
  if (result === null) return null
  const tone = result === 'failed' ? 'bad' : result === 'updated' ? 'warn' : 'ok'
  return <span className={`pill ${tone}`}>{result}</span>
}

interface AddApiProps {
  server: string
  onCancel: () => void
  onDone: (imported: ImportedApi[]) => void
}

function AddApi({ server, onCancel, onDone }: AddApiProps) {
  const spec = useSpecSource()
  const [templates, setTemplates] = useState<Template[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    getTemplates().then(setTemplates, (e: unknown) => setError(message(e)))
  }, [])

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    if (!spec.ready) return
    setBusy(true)
    setError(null)
    try {
      onDone(await importApi(server, spec.source()))
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className="panel wizard" onSubmit={submit}>
      <h2>Add API</h2>
      <SpecSourceFields spec={spec} disabled={busy} templates={templates} />
      <small className="muted">An API with the same name as an existing API replaces it.</small>
      {error && (
        <p className="alert" role="alert">
          {error}
        </p>
      )}
      <div className="actions">
        <button type="button" className="btn" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
        <button type="submit" className="btn primary" disabled={!spec.ready || busy}>
          {busy ? 'Importing…' : 'Import'}
        </button>
      </div>
    </form>
  )
}

interface ImportReportProps {
  title: string
  apis: ImportedApi[]
  onClose: () => void
}

function ImportReport({ title, apis, onClose }: ImportReportProps) {
  return (
    <section className="panel callout" aria-live="polite">
      <div className="report">
        {apis.map((api) => (
          <div key={api.name}>
            <h2>
              {title} of {api.name} <small>{api.tools.length} spec tools now</small>
            </h2>
            <Names label="added" names={api.added} />
            <Names label="updated" names={api.changed} />
            <Names label="kept (edited)" names={api.kept} />
            <Names label="removed" names={api.removed} />
            {api.skipped.length > 0 && <Names label="skipped" names={api.skipped.map((s) => `${s.method ?? '*'} ${s.path}`)} />}
          </div>
        ))}
      </div>
      <button className="btn" type="button" onClick={onClose}>
        Close
      </button>
    </section>
  )
}

function Names({ label, names }: { label: string; names: string[] }) {
  return (
    <Row label={label}>
      {names.length === 0 ? <span className="muted">none</span> : <span className="mono api-url">{names.join(', ')}</span>}
    </Row>
  )
}
