// The auth fields of an API. A saved secret shows as `set` and is never sent back.

import type { AuthState, AuthType } from './apiAuth'
import type { ApiView } from './types/ApiView'

interface AuthFieldsProps {
  auth: AuthState
  onChange: (auth: AuthState) => void
  /** The saved API. A saved secret shows as `set` and is never sent back. */
  api: ApiView | undefined
}

export function AuthFields({ auth, onChange, api }: AuthFieldsProps) {
  const secretSaved =
    api !== undefined && auth.type === api.auth.type && 'secret_set' in api.auth && api.auth.secret_set && !auth.replacing
  return (
    <div className="line">
      <select value={auth.type} onChange={(e) => onChange({ ...auth, type: e.target.value as AuthType, secret: '' })} aria-label="Auth type">
        <option value="none">none</option>
        <option value="api_key">API key header</option>
        <option value="bearer">bearer token</option>
      </select>
      {auth.type === 'api_key' && (
        <input className="grow" value={auth.header} onChange={(e) => onChange({ ...auth, header: e.target.value })} placeholder="X-API-Key" aria-label="Auth header name" />
      )}
      {auth.type !== 'none' &&
        (secretSaved ? (
          <>
            <span className="pill ok">set</span>
            <button className="btn" type="button" onClick={() => onChange({ ...auth, replacing: true })}>
              Replace
            </button>
          </>
        ) : (
          <input
            className="grow"
            type="password"
            autoComplete="off"
            value={auth.secret}
            onChange={(e) => onChange({ ...auth, secret: e.target.value })}
            placeholder="Secret"
            aria-label="Auth secret"
          />
        ))}
    </div>
  )
}
