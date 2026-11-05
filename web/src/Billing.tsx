// "Plan & billing" page: the plan cards, the usage of this period, and the subscription state.
// Stripe manages the subscription. This page only shows the state from the database and has
// two buttons: Checkout to start a subscription, and the Customer Portal for all other changes.

import { useEffect, useState } from 'react'
import { ApiError, openBillingPortal, startCheckout } from './api'
import { PixelScenes } from './PixelScenes'
import { PlanTable } from './PlanTable'
import {
  dayText,
  historyText,
  limitText,
  plural,
  quotaShare,
  quotaTone,
  usePlan,
  usePlans,
  type Billing as BillingData,
  type Plan,
} from './plans'

const NOT_SET_UP = 'Payments are not set up yet'

// The webhook can arrive after the redirect from Checkout, so the page asks again a few times.
const CHECKOUT_POLLS = 10
const CHECKOUT_POLL_MS = 2_000

const message = (error: unknown) => (error instanceof ApiError || error instanceof Error ? error.message : String(error))

function takeCheckoutFlag(): boolean {
  const params = new URLSearchParams(window.location.search)
  return params.get('checkout') === 'success'
}

function dropCheckoutFlag() {
  const params = new URLSearchParams(window.location.search)
  params.delete('checkout')
  const query = params.toString()
  window.history.replaceState(window.history.state, '', window.location.pathname + (query ? `?${query}` : ''))
}

/** After Checkout, asks for the plan until it is a paid plan or the polls run out. */
function useCheckoutReturn(billing: BillingData | null, reload: () => void): 'waiting' | 'done' | null {
  const [returned, setReturned] = useState(takeCheckoutFlag)
  const paid = billing !== null && !billing.plan.is_default

  useEffect(() => {
    if (!returned) return
    if (paid) {
      dropCheckoutFlag()
      return
    }
    let polls = 0
    const timer = window.setInterval(() => {
      polls += 1
      reload()
      if (polls >= CHECKOUT_POLLS) {
        window.clearInterval(timer)
        dropCheckoutFlag()
        setReturned(false)
      }
    }, CHECKOUT_POLL_MS)
    return () => window.clearInterval(timer)
  }, [returned, paid, reload])
  if (!returned) return null
  return paid ? 'done' : 'waiting'
}

export function Billing() {
  const { billing, reload } = usePlan()
  const { plans, error: plansError } = usePlans()
  const [busy, setBusy] = useState(false)
  const [actionError, setActionError] = useState<string | null>(null)
  const checkout = useCheckoutReturn(billing, reload)

  // The shell has the plan already. The page loads it again, so the usage is fresh.
  useEffect(reload, [reload])

  const go = async (load: () => Promise<{ url: string }>) => {
    setBusy(true)
    setActionError(null)
    try {
      window.location.assign((await load()).url)
    } catch (e) {
      setActionError(message(e))
      setBusy(false)
    }
  }

  const configured = billing?.billing_configured ?? false
  // A canceled subscription is over. The user can then start a new one with Checkout.
  const status = billing?.subscription_status
  const subscribed = status != null && status !== 'canceled' && status !== 'incomplete_expired'

  const action = (plan: Plan) => {
    if (!billing) return null
    if (plan.code === billing.plan.code) {
      return billing.has_customer ? (
        <ManageButton configured={configured} busy={busy} onClick={() => go(openBillingPortal)} />
      ) : null
    }
    // Checkout only starts the first subscription. The Portal does every later change.
    if (plan.price_cents === 0 || subscribed) return null
    return (
      <>
        <button
          className="btn primary"
          type="button"
          disabled={!configured || busy}
          onClick={() => go(() => startCheckout(plan.code))}
        >
          Upgrade to {plan.name}
        </button>
        {!configured && <span className="note">{NOT_SET_UP}</span>}
      </>
    )
  }

  return (
    <>
      <div className="head">
        <div>
          <h1>Plan &amp; billing</h1>
          <div className="sub">
            {billing ? `You are on the ${billing.plan.name} plan. ` : ''}The limits apply to all servers of your account.
          </div>
        </div>
        <PixelScenes />
      </div>

      {checkout === 'waiting' && (
        <p className="note" role="status">
          Payment received. Waiting for Stripe to confirm the plan…
        </p>
      )}
      {checkout === 'done' && billing && (
        <p className="plan-banner ok" role="status">
          You are on the {billing.plan.name} plan now.
        </p>
      )}
      {actionError && (
        <p className="alert" role="alert">
          {actionError}
        </p>
      )}

      {billing && <Usage billing={billing} />}
      {billing && subscribed && <Subscription billing={billing} />}

      <section aria-label="Plans">
        {plansError && (
          <p className="alert" role="alert">
            {plansError}
          </p>
        )}
        {plans && <PlanTable plans={plans} current={billing?.plan.code} action={action} />}
      </section>
    </>
  )
}

function ManageButton({ configured, busy, onClick }: { configured: boolean; busy: boolean; onClick: () => void }) {
  return (
    <>
      <button className="btn" type="button" disabled={!configured || busy} onClick={onClick}>
        Manage billing
      </button>
      {!configured && <span className="note">{NOT_SET_UP}</span>}
    </>
  )
}

function Usage({ billing }: { billing: BillingData }) {
  const max = billing.plan.max_calls_per_month
  const tone = quotaTone(billing)
  const share = quotaShare(billing)
  const reset = dayText(billing.resets_at_ms)
  const paused = [
    billing.paused_servers > 0 ? plural(billing.paused_servers, 'server') : null,
    billing.paused_tools > 0 ? plural(billing.paused_tools, 'tool') : null,
  ].filter(Boolean)

  return (
    <section className="panel usage" aria-label="Usage">
      <h2>Usage this period</h2>
      <div className="usage-grid">
        <div className="usage-item wide">
          <div className="label">Tool calls</div>
          <div className="value">
            {limitText(billing.calls_used)}
            <small> / {limitText(max)}</small>
          </div>
          {max !== null && (
            <div
              className={`usage-bar ${tone}`}
              role="progressbar"
              aria-label="Tool calls this period"
              aria-valuemin={0}
              aria-valuemax={max}
              aria-valuenow={Math.min(billing.calls_used, max)}
            >
              <span style={{ width: `${share * 100}%` }} />
            </div>
          )}
          <div className={tone === 'ok' ? 'delta' : `delta ${tone}-text`}>
            {tone === 'bad' ? `Limit reached. Calls stop until ${reset}.` : `Resets on ${reset}`}
          </div>
        </div>
        <div className="usage-item">
          <div className="label">MCP servers</div>
          <div className="value">
            {billing.servers_used}
            <small> / {limitText(billing.plan.max_servers)}</small>
          </div>
          <div className={paused.length ? 'delta warn-text' : 'delta'}>
            {paused.length ? `${paused.join(' and ')} paused` : 'none paused'}
          </div>
        </div>
        <div className="usage-item">
          <div className="label">Call history</div>
          <div className="value">{historyText(billing.plan.history_hours)}</div>
          <div className="delta">older calls are deleted</div>
        </div>
      </div>
    </section>
  )
}

function Subscription({ billing }: { billing: BillingData }) {
  const end = billing.current_period_end_ms
  let text: string
  if (billing.subscription_status === 'past_due') {
    text = 'The last payment failed. Stripe tries again. Update the card in Manage billing.'
  } else if (billing.cancel_at_period_end && end !== null) {
    text = `Cancels on ${dayText(end)}. The plan stays until then.`
  } else if (end !== null) {
    text = `Renews on ${dayText(end)}.`
  } else {
    text = `Subscription status: ${billing.subscription_status}.`
  }
  return (
    <section className="panel subscription">
      <h2>{billing.plan.name} subscription</h2>
      <p className={billing.subscription_status === 'past_due' ? 'warn-text' : 'muted'}>{text}</p>
    </section>
  )
}
