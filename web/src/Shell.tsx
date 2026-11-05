// App shell: side menu with the theme picker, header, and a small router for the pages.

import { useCallback, useEffect, useState } from 'react'
import { getBilling, getServers, setTheme, signOut, type Theme, type User } from './api'
import { Brand } from './Brand'
import { Apis } from './Apis'
import { Billing } from './Billing'
import { dayText, PlanContext, plural, quotaTone, type Billing as BillingData, type PlanState } from './plans'
import { Builder } from './Builder'
import { Live } from './Live'
import { Onboarding } from './Onboarding'
import { Servers } from './Servers'
import { Tokens } from './Tokens'
import { Tools } from './Tools'
import type { ServerSummary } from './types/ServerSummary'

const PAGES: { path: string; label: string }[] = [
  { path: '/', label: 'Live' },
  { path: '/apis', label: 'APIs' },
  { path: '/tools', label: 'Tools' },
  { path: '/tokens', label: 'Tokens' },
  { path: '/servers', label: 'All servers' },
  { path: '/billing', label: 'Plan & billing' },
]

// MCP calls change the usage without an action in the dashboard, so the shell asks again from time to time.
const BILLING_REFRESH_MS = 60_000

const THEMES: { id: Theme; label: string }[] = [
  { id: 'neon', label: 'Neon' },
  { id: 'calm', label: 'Calm' },
  { id: 'light', label: 'Light' },
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

/**
 * Colors fade only during a theme change. A global color transition would also run on the
 * first page load, when the saved theme replaces the default theme.
 */
function fadeTheme() {
  const root = document.documentElement
  root.classList.add('theme-change')
  window.setTimeout(() => root.classList.remove('theme-change'), 300)
}

/** Routes of the tool builder: /servers/<name>/tools/new and /servers/<name>/tools/<tool>. */
function builderRoute(path: string): { server: string; tool: string | null } | null {
  const match = /^\/servers\/([^/]+)\/tools\/([^/]+)$/.exec(path)
  if (!match) return null
  const [server, tool] = [decodeURIComponent(match[1]), decodeURIComponent(match[2])]
  return { server, tool: tool === 'new' ? null : tool }
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
    // The builder reads `from` for its back link. A move between builder pages keeps the first page.
    const current = window.location.pathname
    const from = current.startsWith('/servers/') ? (window.history.state?.from ?? '/') : current
    window.history.pushState({ from }, '', next)
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

  const [billing, setBilling] = useState<BillingData | null>(null)

  // A failed plan load keeps the last value. The pages then show no plan details.
  const reloadBilling = useCallback(() => {
    getBilling().then(setBilling, () => {})
  }, [])
  useEffect(() => {
    reloadBilling()
    const timer = window.setInterval(reloadBilling, BILLING_REFRESH_MS)
    return () => window.clearInterval(timer)
  }, [reloadBilling])

  // Each action that changes servers or tools calls this, so it also loads the plan usage again.
  const reloadServers = useCallback(() => {
    getServers().then(
      (list) => {
        setServers(list)
        setLoadError(null)
      },
      (e: Error) => setLoadError(e.message),
    )
    reloadBilling()
  }, [reloadBilling])
  useEffect(reloadServers, [reloadServers])
  const planState: PlanState = { billing, reload: reloadBilling }
  const atServerLimit =
    billing !== null &&
    servers !== null &&
    billing.plan.max_servers !== null &&
    servers.length >= billing.plan.max_servers

  const select = (name: string) => {
    setSelectedName(name)
    saveSelected(name)
  }

  const changeTheme = async (theme: Theme) => {
    const previous = user.theme
    fadeTheme()
    onUserChange({ ...user, theme })
    setThemeError(null)
    try {
      await setTheme(theme)
    } catch {
      fadeTheme()
      onUserChange({ ...user, theme: previous })
      setThemeError('Could not save the theme.')
    }
  }

  const handleSignOut = async () => {
    await signOut()
    onSignedOut()
  }

  const route = builderRoute(path)
  const selected =
    servers?.find((s) => s.name === (route?.server ?? selectedName)) ?? servers?.[0] ?? null
  const selectedTitle = selected?.name
  useEffect(() => {
    document.title = selectedTitle ? `ToolPort · ${selectedTitle}` : 'ToolPort'
    return () => {
      document.title = 'ToolPort'
    }
  }, [selectedTitle])
  const renamed = (oldName: string, newName: string) => {
    if (selected?.name === oldName) select(newName)
    reloadServers()
  }
  // After a delete of the selected server, the next server gets selected. With no server
  // left, the shell shows the onboarding.
  const deleted = (name: string) => {
    const next = servers?.find((s) => s.name !== name)
    if (selected?.name === name && next) select(next.name)
    reloadServers()
  }
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
  } else if (path === '/billing') {
    content = <Billing />
  } else if (route && !servers.some((s) => s.name === route.server)) {
    content = (
      <p className="alert" role="alert">
        The server {route.server} does not exist.
      </p>
    )
  } else if (selected && path === '/new' && atServerLimit && billing) {
    content = <ServerLimit billing={billing} navigate={navigate} />
  } else if (!selected || path === '/new') {
    content = (
      <Onboarding
        key="new"
        onCancel={selected ? () => navigate('/') : undefined}
        onChanged={reloadServers}
        onDone={finishOnboarding}
      />
    )
  } else if ((selected.apis === 0 && path !== '/servers') || path === '/connect') {
    // An earlier onboarding stopped before the import, or the user wants a new client token.
    // The All servers page stays open, so the user can delete an unfinished server.
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
    content = route ? (
      <Builder
        key={path}
        server={selected.name}
        tool={route.tool}
        navigate={navigate}
        onChanged={reloadServers}
      />
    ) : page.path === '/apis' ? (
        <Apis key={selected.name} server={selected.name} onChanged={reloadServers} />
      ) : page.path === '/tools' ? (
        <Tools key={selected.name} server={selected.name} navigate={navigate} onChanged={reloadServers} />
      ) : page.path === '/tokens' ? (
        <Tokens key={selected.name} server={selected} onChanged={reloadServers} />
      ) : page.path === '/servers' ? (
        <Servers
          servers={servers}
          atLimit={atServerLimit}
          selected={selected.name}
          onOpen={(name) => {
            select(name)
            navigate('/')
          }}
          onNew={() => navigate('/new')}
          onOpenBilling={() => navigate('/billing')}
          onRenamed={renamed}
          onDeleted={deleted}
          onRefresh={reloadServers}
        />
      ) : (
        <Live
          key={selected.name}
          server={selected}
          onToolsChanged={reloadServers}
          onConnectClient={() => navigate('/connect')}
          onOpenTool={(tool) =>
            navigate(`/servers/${encodeURIComponent(selected.name)}/tools/${encodeURIComponent(tool ?? 'new')}`)
          }
        />
      )
  }

  const navCount: Record<string, number | undefined> = {
    '/apis': selected?.apis,
    '/tools': selected?.enabled_tools,
    '/tokens': selected?.active_tokens,
    '/servers': servers?.length,
  }

  return (
    <PlanContext.Provider value={planState}>
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
          <div className="theme-pick">
            <label>
              Theme{' '}
              <select value={user.theme} onChange={(e) => changeTheme(e.target.value as Theme)}>
                {THEMES.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.label}
                  </option>
                ))}
              </select>
            </label>
            {themeError && (
              <p className="alert" role="alert">
                {themeError}
              </p>
            )}
          </div>
          <div className="user">
            {user.avatar_url ? (
              <img className="avatar" src={user.avatar_url} alt="" />
            ) : (
              <div className="avatar">{user.github_login.slice(0, 2).toUpperCase()}</div>
            )}
            <span className="user-name" title={user.name ?? undefined}>
              {user.github_login}
            </span>
            {billing && (
              <a
                className={`plan-badge${billing.plan.is_default ? '' : ' paid'}`}
                href="/billing"
                title="Plan & billing"
                onClick={(event) => {
                  event.preventDefault()
                  navigate('/billing')
                }}
              >
                {billing.plan.name}
              </a>
            )}
            <button className="btn" type="button" onClick={handleSignOut}>
              Sign out
            </button>
          </div>
        </nav>

        {/* A new key on each page change restarts the page enter animation. */}
        <main key={servers ? path : 'loading'}>
          {billing && path !== '/billing' && <PlanBanner billing={billing} navigate={navigate} />}
          {content}
        </main>
      </div>
    </PlanContext.Provider>
  )
}

/** One line about the quota and the paused servers and tools. It is empty when all is well. */
function bannerText(billing: BillingData): { text: string; tone: 'warn' | 'bad' } | null {
  const tone = quotaTone(billing)
  const max = billing.plan.max_calls_per_month
  if (tone === 'bad') {
    return {
      text: `The monthly limit of ${plural(max ?? 0, 'tool call')} is reached. Calls stop until ${dayText(billing.resets_at_ms)}.`,
      tone,
    }
  }
  const parts: string[] = []
  if (tone === 'warn') parts.push(`You used ${billing.calls_used} of ${max} tool calls this month.`)
  const paused = [
    billing.paused_servers > 0 ? plural(billing.paused_servers, 'server') : null,
    billing.paused_tools > 0 ? plural(billing.paused_tools, 'tool') : null,
  ].filter(Boolean)
  if (paused.length) {
    const verb = billing.paused_servers + billing.paused_tools === 1 ? 'is' : 'are'
    parts.push(`${paused.join(' and ')} ${verb} paused by the ${billing.plan.name} plan.`)
  }
  return parts.length ? { text: parts.join(' '), tone: 'warn' } : null
}

function PlanBanner({ billing, navigate }: { billing: BillingData; navigate: (path: string) => void }) {
  const banner = bannerText(billing)
  if (!banner) return null
  return (
    <p className={`plan-banner ${banner.tone}`} role="status">
      <span>{banner.text}</span>
      <a
        href="/billing"
        onClick={(event) => {
          event.preventDefault()
          navigate('/billing')
        }}
      >
        Plan &amp; billing
      </a>
    </p>
  )
}

/** Shown instead of the onboarding when the plan allows no more servers. */
function ServerLimit({ billing, navigate }: { billing: BillingData; navigate: (path: string) => void }) {
  const max = billing.plan.max_servers ?? 0
  return (
    <section className="panel callout">
      <div>
        <h2>No room for a new server</h2>
        <p className="muted">
          The {billing.plan.name} plan allows {plural(max, 'server')}. Upgrade your plan or delete a server first.
        </p>
      </div>
      <div className="head-actions">
        <button className="btn" type="button" onClick={() => navigate('/servers')}>
          All servers
        </button>
        <button className="btn primary" type="button" onClick={() => navigate('/billing')}>
          See the plans
        </button>
      </div>
    </section>
  )
}
