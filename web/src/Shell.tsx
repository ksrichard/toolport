import { useEffect, useState } from 'react'
import { setTheme, signOut, type Theme, type User } from './api'
import { Brand } from './Brand'

interface Page {
  path: string
  label: string
  title: string
  milestone: number
  summary: string
}

const PAGES: Page[] = [
  { path: '/', label: 'Live', title: 'Live', milestone: 7, summary: 'Live calls, metrics, and connected clients for the selected server.' },
  { path: '/apis', label: 'APIs', title: 'APIs', milestone: 9, summary: 'Import OpenAPI specs and set the upstream base URL and auth.' },
  { path: '/tools', label: 'Tools', title: 'Tools', milestone: 8, summary: 'Turn single tools on or off. Agents do not see tools that are off.' },
  { path: '/tokens', label: 'Tokens', title: 'Tokens', milestone: 10, summary: 'Create and revoke the tokens that MCP clients use to connect.' },
  { path: '/servers', label: 'All servers', title: 'All servers', milestone: 11, summary: 'Create, rename, and delete your MCP servers.' },
]

const THEMES: { id: Theme; label: string }[] = [
  { id: 'neon', label: 'Neon terminal' },
  { id: 'calm', label: 'Calm dark' },
  { id: 'light', label: 'Clean light' },
]

/** Small client-side router. Caddy and Vite serve index.html for every page path. */
function usePath(): [string, (path: string) => void] {
  const [path, setPath] = useState(window.location.pathname)
  useEffect(() => {
    const onPop = () => setPath(window.location.pathname)
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])
  const navigate = (next: string) => {
    window.history.pushState(null, '', next)
    setPath(next)
  }
  return [path, navigate]
}

interface ShellProps {
  user: User
  onUserChange: (user: User) => void
  onSignedOut: () => void
}

export function Shell({ user, onUserChange, onSignedOut }: ShellProps) {
  const [path, navigate] = usePath()
  const [themeError, setThemeError] = useState<string | null>(null)
  const page = PAGES.find((p) => p.path === path) ?? PAGES[0]

  const changeTheme = async (theme: Theme) => {
    const previous = user.theme
    onUserChange({ ...user, theme })
    setThemeError(null)
    try {
      await setTheme(theme)
    } catch {
      onUserChange({ ...user, theme: previous })
      setThemeError('Could not save the theme.')
    }
  }

  const handleSignOut = async () => {
    await signOut()
    onSignedOut()
  }

  return (
    <div className="app">
      <nav className="side" aria-label="Main">
        <Brand />
        <ul>
          {PAGES.map((p) => (
            <li key={p.path}>
              <a
                href={p.path}
                aria-current={p === page ? 'page' : undefined}
                onClick={(event) => {
                  event.preventDefault()
                  navigate(p.path)
                }}
              >
                {p.label}
              </a>
            </li>
          ))}
        </ul>
        <div className="user">
          {user.avatar_url ? (
            <img className="avatar" src={user.avatar_url} alt="" />
          ) : (
            <div className="avatar">{user.github_login.slice(0, 2).toUpperCase()}</div>
          )}
          <span className="user-name" title={user.name ?? undefined}>
            {user.github_login}
          </span>
          <button className="btn" type="button" onClick={handleSignOut}>
            Sign out
          </button>
        </div>
      </nav>

      <main>
        <div className="head">
          <h1>{page.title}</h1>
          <div className="skins" role="group" aria-label="Theme">
            {THEMES.map((t) => (
              <button
                key={t.id}
                type="button"
                aria-pressed={user.theme === t.id}
                onClick={() => changeTheme(t.id)}
              >
                {t.label}
              </button>
            ))}
          </div>
        </div>
        {themeError && (
          <p className="alert" role="alert">
            {themeError}
          </p>
        )}
        <section className="panel">
          <h2>Coming in milestone {page.milestone}</h2>
          <p className="muted">{page.summary}</p>
        </section>
      </main>
    </div>
  )
}
