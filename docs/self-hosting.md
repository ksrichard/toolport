# Run your own ToolPort

This guide sets up your own ToolPort in three steps:

1. [Run it on your computer](#1-run-it-on-your-computer).
2. [Turn on payments with Stripe test mode](#2-turn-on-payments-stripe-test-mode). This step is optional.
3. [Put it on your own server](#3-put-it-on-your-own-server). This step is optional too.

Each step uses the one before it.

## What you need

- Docker with Compose v2
- A GitHub account, for the sign-in
- `openssl`, to make random secrets
- For step 2: a free Stripe account and `jq`
- For step 3: a Linux server (VPS) with a public IP, and a domain name

## 1. Run it on your computer

### Make a GitHub OAuth app

The dashboard has only one sign-in method: GitHub.

1. Open https://github.com/settings/developers and click "New OAuth app".
2. Fill in the form:
   - Homepage URL: `https://localhost`
   - Authorization callback URL: `https://localhost/auth/github/callback`
3. Click "Generate a new client secret". Keep the client ID and the secret for the next step.

### Make the `.env` file

```sh
cp .env.example .env
```

Change these values in `.env`:

| Variable | Value |
| --- | --- |
| `POSTGRES_PASSWORD` | A new password. Put the same password in `DATABASE_URL`. |
| `JWT_SECRET` | The output of `openssl rand -hex 32`. |
| `SECRETS_KEY` | A second, different output of `openssl rand -hex 32`. It must have 64 hex characters. |
| `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET` | The values of your OAuth app. |

Git ignores `.env`. Never commit it.

### Start it

```sh
docker compose up --build
```

Open https://localhost and sign in with GitHub.
Your browser warns about the local certificate the first time.
Caddy makes this certificate for `localhost`, so you can accept it.

The first sign-in creates your user, and a wizard helps you make your first server.

## 2. Turn on payments (Stripe test mode)

Without Stripe, ToolPort works with the Free plan limits, and the billing pages show that payments are off.
Stripe test mode uses fake cards, so no real money moves.

1. Make a Stripe account. Open https://dashboard.stripe.com/test/apikeys and copy the secret key (`sk_test_...`).
2. Put the key in `.env`:
   ```sh
   STRIPE_SECRET_KEY=sk_test_...
   ```
3. Create the Stripe product, price, and Customer Portal settings:
   ```sh
   scripts/stripe-setup.sh
   ```
   The script creates the product `toolport_pro` and a $9/month price with the lookup key `toolport_pro_monthly`.
   The server finds the price by this lookup key.
   You can run the script again safely. It refuses live keys.
4. Write the webhook secret into `.env`:
   ```sh
   scripts/stripe-webhook-secret.sh
   ```
   The Stripe CLI runs in Docker and sends the Stripe events to your local server.
   This secret only works for this local Stripe CLI.
5. Start ToolPort with the Stripe CLI:
   ```sh
   docker compose --profile stripe up --build
   ```
6. In the dashboard, open "Plan & billing" and click "Upgrade to Pro".
   Pay with the test card `4242 4242 4242 4242`, any future date, and any CVC.
   The page shows Pro a few seconds later, when the webhook arrives.
7. Click "Manage billing" to open the Stripe Customer Portal. A cancel there takes effect at the end of the period.

To see the webhook deliveries:

```sh
docker compose logs -f stripe-cli server
```

## 3. Put it on your own server

The same `compose.yaml` runs on the server, with `compose.prod.yaml` on top.
`.github/workflows/deploy.yml` builds the images in GitHub Actions, pushes them to GHCR, and starts them on the server.

### Prepare the domain and the server

1. Point an `A` record of your domain (for example `toolport.example.com`) to the server IP.
   If you use Cloudflare, turn off the proxy for this record. Caddy needs a direct connection to get the Let's Encrypt certificate.
2. Install Docker on the server, and open the ports 80 and 443.
3. As root, make a deploy user and the folders:
   ```sh
   adduser --disabled-password deploy
   usermod -aG docker deploy
   install -d -o deploy -g deploy /opt/toolport
   install -d /var/lib/toolport/postgres /var/lib/toolport/caddy/data /var/lib/toolport/caddy/config
   ```
   The database and the certificates live in `/var/lib/toolport`.
   No Docker command deletes these host folders.
   When a folder is missing, the start stops, so the server never starts with an empty database.
4. On your computer, make an SSH key for the deploy, and add its public key to the server:
   ```sh
   ssh-keygen -t ed25519 -f toolport-deploy -N ""
   ssh-copy-id -i toolport-deploy.pub deploy@<server-ip>
   ssh-keyscan <server-ip> > toolport-known-hosts
   ```

### Make a second GitHub OAuth app

A GitHub OAuth app allows only one callback URL. Make a new app for production:

- Homepage URL: `https://toolport.example.com`
- Authorization callback URL: `https://toolport.example.com/auth/github/callback`

### Add the settings to your GitHub repo

Open your fork on GitHub, then "Settings" > "Secrets and variables" > "Actions".

| Name | Kind | Value |
| --- | --- | --- |
| `VPS_HOST` | Variable | The server IP. |
| `TOOLPORT_DOMAIN` | Variable | The domain, for example `toolport.example.com`. |
| `VPS_SSH_KEY` | Secret | The content of `toolport-deploy` (the private key). |
| `VPS_KNOWN_HOSTS` | Secret | The content of `toolport-known-hosts`. |
| `POSTGRES_PASSWORD` | Secret | A new password. |
| `JWT_SECRET` | Secret | A new `openssl rand -hex 32` value. |
| `SECRETS_KEY` | Secret | A new `openssl rand -hex 32` value. Never change it later, because it decrypts the stored upstream secrets. |
| `OAUTH_GITHUB_CLIENT_ID`, `OAUTH_GITHUB_CLIENT_SECRET` | Secret | The values of the production OAuth app. GitHub does not allow secret names that start with `GITHUB_`. |
| `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET` | Secret | Optional. See the next section. |

Use new values for production. Do not copy the values from your local `.env`.
You can set them with the GitHub CLI too. The value comes from stdin, so it does not show in your shell history:

```sh
openssl rand -hex 32 | gh secret set JWT_SECRET
gh secret set VPS_SSH_KEY < toolport-deploy
gh variable set TOOLPORT_DOMAIN --body toolport.example.com
```

### Payments on the server

The local webhook secret from step 2 does not work on the server.
The server needs its own Stripe webhook endpoint:

1. In the Stripe dashboard, open "Developers" > "Webhooks" and add an endpoint.
2. URL: `https://toolport.example.com/api/billing/webhook`
3. Events:
   - `checkout.session.completed`
   - `customer.subscription.created`, `.updated`, `.deleted`, `.paused`, `.resumed`
   - `invoice.paid`, `invoice.payment_failed`
4. Copy the signing secret (`whsec_...`) of this endpoint into the repo secret `STRIPE_WEBHOOK_SECRET`.
5. Put the Stripe secret key into the repo secret `STRIPE_SECRET_KEY`.

With a test key, the server takes only test cards.
For real payments, switch the Stripe dashboard to live mode.
In live mode, create the product, the price with the lookup key `toolport_pro_monthly`, the Customer Portal settings, and the webhook endpoint again.
Then use the live key and the live webhook secret.
`scripts/stripe-setup.sh` refuses live keys, so do these steps in the Stripe dashboard.

### Deploy

1. Open the "Actions" tab of your fork. Turn on the "Deploy" workflow if GitHub shows it as off.
2. Push to `main`. When CI passes, the Deploy workflow runs.
   You can also start it by hand: "Actions" > "Deploy" > "Run workflow".
3. Open `https://toolport.example.com` and sign in.

The workflow writes a new `.env` to `/opt/toolport` on each deploy, so change the settings only in GitHub.
It checks `/api/health` at the end, and it shows the server logs when the check fails.

### Look after the server

- Logs: `ssh deploy@<server-ip> 'cd /opt/toolport && docker compose -f compose.yaml -f compose.prod.yaml logs -f server'`
- The data lives in host folders, so `docker compose down -v` and `docker volume prune` do not delete it. Still, do not run them on the server.
- ToolPort has no backup job yet. Back up `/var/lib/toolport/postgres` with `pg_dump` on your own schedule.
