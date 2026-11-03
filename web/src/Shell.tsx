// App shell: side menu, header, theme switcher, and a small router for the pages.

import { useCallback, useEffect, useState } from 'react'
import { getServers, setTheme, signOut, type Theme, type User } from './api'
import { Brand } from './Brand'
import { Live } from './Live'
import { Onboarding } from './Onboarding'
import type { ServerSummary } from './types/ServerSummary'

interface Page {
  path: string
  label: string
  milestone: number
  summary: string
}

const PAGES: Page[] = [
  { path: '/', label: 'Live', milestone: 7, summary: '' },
  { path: '/apis', label: 'APIs', milestone: 9, summary: 'Import OpenAPI specs and set the upstream base URL and auth.' },
  { path: '/tools', label: 'Tools', milestone: 8, summary: 'Turn single tools on or off. Agents do not see tools that are off.' },
  { path: '/tokens', label: 'Tokens', milestone: 10, summary: 'Create and revoke the tokens that MCP clients use to connect.' },
  { path: '/servers', label: 'All servers', milestone: 11, summary: 'Create, rename, and delete your MCP servers.' },
]

const THEMES: { id: Theme; label: string }[] = [
  { id: 'neon', label: 'Neon terminal' },
  { id: 'calm', label: 'Calm dark' },
  { id: 'light', label: 'Clean light' },
]

/** The browser remembers the last server. It is a convenience only, so errors are ignored. */
const SELECTED_KEY = 'toolport.server'
function loadSelected(): string | null {
  try {
    return localStorage.getItem(SELECTED_KEY)
  } catch {
    return null
  }
}
function saveSelected(name: string) {
  try {
    localStorage.setItem(SELECTED_KEY, name)
  } catch {
    // Private windows can block storage.
  }
}

/** Small client-side router. Caddy and Vite serve index.html for every page path. */
function usePath(): [string, (path: string) => void] {
  const [path, setPath] = useState(window.location.pathname)
  useEffect(() => {
    const onPop = () => setPath(window.location.pathname)
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])
  const navigate = useCallback((next: string) => {
    window.history.pushState(null, '', next)
    setPath(next)
  }, [])
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
  const [servers, setServers] = useState<ServerSummary[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [selectedName, setSelectedName] = useState<string | null>(loadSelected)

  const reloadServers = useCallback(() => {
    getServers().then(
      (list) => {
        setServers(list)
        setLoadError(null)
      },
      (e: Error) => setLoadError(e.message),
    )
  }, [])
  useEffect(reloadServers, [reloadServers])

  const select = (name: string) => {
    setSelectedName(name)
    saveSelected(name)
  }

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

  const selected = servers?.find((s) => s.name === selectedName) ?? servers?.[0] ?? null
  const finishOnboarding = (name: string) => {
    select(name)
    reloadServers()
    navigate('/')
  }

  let content
  if (loadError) {
    content = (
      <p className="alert" role="alert">
        Could not load your servers: {loadError}
      </p>
    )
  } else if (!servers) {
    content = null
  } else if (!selected || path === '/new') {
    content = (
      <Onboarding
        key="new"
        onCancel={selected ? () => navigate('/') : undefined}
        onChanged={reloadServers}
        onDone={finishOnboarding}
      />
    )
  } else if (selected.apis === 0 || path === '/connect') {
    // An earlier onboarding stopped before the import, or the user wants a new client token.
    content = (
      <Onboarding
        key={`resume/${selected.name}`}
        existing={selected}
        onCancel={selected.apis > 0 ? () => navigate('/') : undefined}
        onChanged={reloadServers}
        onDone={finishOnboarding}
      />
    )
  } else {
    const page = PAGES.find((p) => p.path === path) ?? PAGES[0]
    content =
      page.path === '/' ? (
        <Live key={selected.name} server={selected} onConnectClient={() => navigate('/connect')} />
      ) : (
        <>
          <div className="head">
            <h1>{page.label}</h1>
          </div>
          <section className="panel">
            <h2>Coming in milestone {page.milestone}</h2>
            <p className="muted">{page.summary}</p>
          </section>
        </>
      )
  }

  const navCount: Record<string, number | undefined> = {
    '/apis': selected?.apis,
    '/tools': selected?.enabled_tools,
    '/tokens': selected?.active_tokens,
    '/servers': servers?.length,
  }

  return (
    <>
      <div className="switcher">
        <div className="skins" role="group" aria-label="Theme">
          {THEMES.map((t) => (
            <button key={t.id} type="button" aria-pressed={user.theme === t.id} onClick={() => changeTheme(t.id)}>
              {t.label}
            </button>
          ))}
        </div>
        {themeError && (
          <p className="alert" role="alert">
            {themeError}
          </p>
        )}
      </div>
      <div className="app">
        <nav className="side" aria-label="Main">
          <Brand />
          {selected && (
            <div>
              <div className="group">
                {servers && servers.length > 1 ? (
                  <label>
                    Server{' '}
                    <select value={selected.name} onChange={(e) => select(e.target.value)}>
                      {servers.map((s) => (
                        <option key={s.name}>{s.name}</option>
                      ))}
                    </select>
                  </label>
                ) : (
                  <>Server: {selected.name}</>
                )}
              </div>
              <ul>
                {PAGES.map((p) => (
                  <li key={p.path}>
                    <a
                      href={p.path}
                      aria-current={p.path === path ? 'page' : undefined}
                      onClick={(event) => {
                        event.preventDefault()
                        navigate(p.path)
                      }}
                    >
                      {p.label}
                      {navCount[p.path] !== undefined && <span>{navCount[p.path]}</span>}
                    </a>
                  </li>
                ))}
                <li>
                  <a
                    href="/new"
                    aria-current={path === '/new' ? 'page' : undefined}
                    onClick={(event) => {
                      event.preventDefault()
                      navigate('/new')
                    }}
                  >
                    + New server
                  </a>
                </li>
              </ul>
            </div>
          )}
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

        <main>{content}</main>
      </div>
    </>
  )
}
