import { useEffect, useState } from 'react'
import { ApiError, getMe, refreshSession, type User } from './api'
import { Login } from './Login'
import { Shell } from './Shell'

type Session =
  | { kind: 'loading' }
  | { kind: 'signedOut' }
  | { kind: 'failed'; message: string }
  | { kind: 'signedIn'; user: User }

async function loadSession(): Promise<Session> {
  if (!(await refreshSession())) return { kind: 'signedOut' }
  try {
    return { kind: 'signedIn', user: await getMe() }
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) return { kind: 'signedOut' }
    return { kind: 'failed', message: error instanceof Error ? error.message : String(error) }
  }
}

export default function App() {
  const [session, setSession] = useState<Session>({ kind: 'loading' })

  useEffect(() => {
    let cancelled = false
    loadSession().then((next) => {
      if (!cancelled) setSession(next)
    })
    return () => {
      cancelled = true
    }
  }, [])

  useEffect(() => {
    document.documentElement.dataset.theme = session.kind === 'signedIn' ? session.user.theme : 'neon'
  }, [session])

  switch (session.kind) {
    case 'loading':
      return null
    case 'signedOut':
      return <Login />
    case 'failed':
      return (
        <main className="login">
          <section className="panel login-card">
            <h1>ToolPort is not available</h1>
            <p className="alert" role="alert">{session.message}</p>
            <button className="btn primary big" type="button" onClick={() => window.location.reload()}>
              Retry
            </button>
          </section>
        </main>
      )
    case 'signedIn':
      return (
        <Shell
          user={session.user}
          onUserChange={(user) => setSession({ kind: 'signedIn', user })}
          onSignedOut={() => setSession({ kind: 'signedOut' })}
        />
      )
  }
}
