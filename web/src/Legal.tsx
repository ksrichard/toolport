import type { ReactNode } from 'react'
import { Brand } from './Brand'
import { SiteFooter } from './Landing'

export type LegalPage = 'terms' | 'privacy'

export function Legal({ page }: { page: LegalPage }) {
  return (
    <div className="site">
      <header className="site-head">
        <Brand />
      </header>
      <main className="legal">
        {page === 'terms' ? <Terms /> : <Privacy />}
      </main>
      <SiteFooter />
    </div>
  )
}

function Draft({ children }: { children: ReactNode }) {
  return (
    <>
      <h1>{children}</h1>
      <p className="draft" role="note">
        Draft — not yet reviewed by a lawyer.
      </p>
    </>
  )
}

function Terms() {
  return (
    <>
      <Draft>Terms of use</Draft>
      <p>
        These terms apply to this ToolPort instance. In these terms, "we" means the operator of this ToolPort
        instance. "You" means the person who uses it.
      </p>

      <h2>What ToolPort does</h2>
      <p>
        ToolPort turns REST APIs into MCP servers. You add an API. ToolPort serves each operation as an MCP tool. AI
        clients connect to your server with a token and call the tools.
      </p>

      <h2>Your account</h2>
      <p>You sign in with GitHub. ToolPort has no passwords. Keep your GitHub account secure.</p>
      <p>
        Keep your MCP tokens secret. A person with a token can call the tools of its server. Revoke a token when you
        do not need it.
      </p>

      <h2>Your APIs</h2>
      <p>
        You add only APIs that you have the right to use. ToolPort sends requests to these APIs for you. You are
        responsible for these requests and for the costs that the API providers charge.
      </p>
      <p>Do not use ToolPort to break the law or to attack other systems.</p>

      <h2>Plans and payment</h2>
      <p>
        The Free plan costs nothing. The Pro plan is a monthly subscription. Stripe handles the payment. Your
        subscription renews each month until you cancel it. You cancel it, change your card, and see your invoices in
        the Stripe customer portal. After a cancel, the Pro plan stays until the end of the paid period.
      </p>

      <h2>No guarantee</h2>
      <p>
        We provide ToolPort "as is". We do not promise that it is always available or free of errors. To the extent
        the law allows, we are not liable for loss that comes from its use.
      </p>

      <h2>End of use</h2>
      <p>
        You can stop using ToolPort at any time. To delete your account, ask the operator. We can block an account
        that breaks these terms.
      </p>

      <h2>Changes</h2>
      <p>We can change these terms. This page always shows the current version.</p>
    </>
  )
}

function Privacy() {
  return (
    <>
      <Draft>Privacy</Draft>
      <p>
        This page tells you what data this ToolPort instance stores and why. The operator of this ToolPort instance
        controls the data.
      </p>

      <h2>Sign-in</h2>
      <p>
        You sign in with GitHub. ToolPort asks GitHub only for your public profile. It stores your GitHub user id,
        your GitHub login, your display name, and the address of your avatar image. ToolPort has no passwords and does
        not store your email address.
      </p>

      <h2>Your servers and APIs</h2>
      <p>
        ToolPort stores the servers, APIs, specs, and tools that you add. It also stores your theme choice.
      </p>
      <p>
        An API can need a secret, such as an API key. ToolPort encrypts each secret with AES-256-GCM before it stores
        it.
      </p>

      <h2>Tokens</h2>
      <p>
        ToolPort shows an MCP token value only once, when you create it. The database keeps the token id, its name, its
        expiry, and the time of its last use. It does not keep the token value.
      </p>

      <h2>Call log</h2>
      <p>
        For each tool call that reaches an upstream API, ToolPort stores the time, the server, the tool, the token, the
        client name, the HTTP status, the latency, and the result. It does not store the call arguments or the
        response body. The live dashboard uses this log.
      </p>
      <p>
        The AI client that you connect, such as Claude from Anthropic, sends the call arguments. ToolPort passes them
        to the upstream API that you added. That client and that API have their own privacy rules.
      </p>

      <h2>Payments</h2>
      <p>
        Stripe handles the payments for the Pro plan. You enter your card on a Stripe page, not in ToolPort. ToolPort
        stores only your Stripe customer id, your Stripe subscription id, and the plan state: the plan, the
        subscription status, the current period, and whether the subscription ends at the period end. ToolPort does
        not store or see your card data. Stripe has its own privacy rules.
      </p>

      <h2>Cookies and browser storage</h2>
      <p>
        ToolPort sets a refresh cookie when you sign in. It is HttpOnly, it lives 30 days, and the browser sends it only
        to the sign-in routes. The database keeps only a SHA-256 hash of it. During sign-in, a second cookie lives for
        10 minutes and protects the sign-in step.
      </p>
      <p>
        The app keeps the short-lived access token in memory only. The browser storage keeps the name of the server you
        last selected. ToolPort uses no tracking or analytics cookies.
      </p>

      <h2>Other services</h2>
      <p>
        GitHub handles the sign-in. Stripe handles the payments. The upstream APIs that you add get the requests for your tools. The pages load
        their fonts from an external font service, which sees your IP address.
      </p>

      <h2>How long we keep data</h2>
      <p>
        ToolPort keeps your data until it is deleted. It deletes call log rows after the history time of your plan: 24
        hours on Free and 30 days on Pro. To delete your account and its data, ask the operator.
      </p>

      <h2>Changes</h2>
      <p>We can change this page. This page always shows the current version.</p>
    </>
  )
}
