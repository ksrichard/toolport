import { useState } from 'react'
import { Brand } from './Brand'
import { PLANS, type Plan } from './plans'

const SIGN_IN = '/auth/github/login'

// Codes come from AuthError::code() in crates/server/src/error.rs.
const LOGIN_ERRORS: Record<string, string> = {
  github_denied: 'GitHub sign-in was cancelled.',
  invalid_state: 'The sign-in link expired or came from another tab. Try again.',
  missing_code: 'GitHub did not send a login code. Try again.',
  github_rejected: 'GitHub rejected the sign-in. Try again.',
  github_unavailable: 'ToolPort cannot reach GitHub right now. Try again later.',
  internal_error: 'Something went wrong on the server. Try again later.',
}

function takeLoginError(): string | null {
  const params = new URLSearchParams(window.location.search)
  const code = params.get('login_error')
  if (!code) return null
  // Remove the code from the address bar, so a reload does not show the error again.
  params.delete('login_error')
  const query = params.toString()
  window.history.replaceState(null, '', window.location.pathname + (query ? `?${query}` : ''))
  return LOGIN_ERRORS[code] ?? 'The sign-in failed. Try again.'
}

const STEPS = [
  { title: 'Add an API', text: 'Import an OpenAPI spec, or build a tool by hand.' },
  { title: 'Connect', text: 'Give Claude Code or another MCP client the server URL and a token.' },
  { title: 'Watch', text: 'See each tool call, its status, and its latency on the live dashboard.' },
]

const count = new Intl.NumberFormat('en-US')

function limit(value: number | null, unit: string): string {
  return value === null ? `Unlimited ${unit}` : `${count.format(value)} ${unit}`
}

export function Landing() {
  const [error] = useState(takeLoginError)

  return (
    <div className="site">
      <header className="site-head">
        <Brand />
        <a className="muted-link" href={SIGN_IN}>
          Sign in
        </a>
      </header>

      <main className="site-main">
        <section className="site-hero" aria-labelledby="hero-title">
          <p className="eyebrow">REST API to MCP server</p>
          <h1 id="hero-title" className="hero">
            ToolPort turns your REST API into an MCP server that AI clients can call.
          </h1>
          {error && (
            <p className="alert" role="alert">
              {error}
            </p>
          )}
          <a className="btn primary big" href={SIGN_IN}>
            <GithubIcon /> Start free
          </a>
          <p className="note">You sign in with your GitHub account.</p>
        </section>

        <section aria-labelledby="how-title">
          <h2 id="how-title" className="site-title">
            How it works
          </h2>
          <ol className="site-steps">
            {STEPS.map((step, i) => (
              <li key={step.title} className="panel">
                <span className="step-no">{String(i + 1).padStart(2, '0')}</span>
                <h3>{step.title}</h3>
                <p className="muted">{step.text}</p>
              </li>
            ))}
          </ol>
        </section>

        <section aria-labelledby="pricing-title">
          <h2 id="pricing-title" className="site-title">
            Pricing
          </h2>
          <div className="plans">
            {PLANS.map((plan) => (
              <PlanCard key={plan.id} plan={plan} />
            ))}
          </div>
          <p className="note">
            These plans are a preview. ToolPort does not apply the limits yet, and it takes no payments yet.
          </p>
        </section>
      </main>

      <SiteFooter />
    </div>
  )
}

function PlanCard({ plan }: { plan: Plan }) {
  const paid = plan.priceEur > 0
  return (
    <article className={`panel plan${paid ? ' featured' : ''}`}>
      <h3>{plan.name}</h3>
      <p className="price">
        €{plan.priceEur} <small>per month</small>
      </p>
      <ul>
        <li>{limit(plan.servers, plan.servers === 1 ? 'server' : 'servers')}</li>
        <li>{limit(plan.tools, 'tools')}</li>
        <li>{limit(plan.callsPerMonth, 'calls per month')}</li>
      </ul>
      {paid ? (
        <button className="btn big" type="button" disabled>
          Coming soon
        </button>
      ) : (
        <a className="btn primary big" href={SIGN_IN}>
          Start free
        </a>
      )}
    </article>
  )
}

export function SiteFooter() {
  return (
    <footer className="site-foot">
      <span>ToolPort</span>
      <nav aria-label="Legal">
        <a href="/terms">Terms</a>
        <a href="/privacy">Privacy</a>
      </nav>
    </footer>
  )
}

function GithubIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" />
    </svg>
  )
}
