#!/usr/bin/env bash
# Writes the webhook signing secret of `stripe listen` into .env as STRIPE_WEBHOOK_SECRET.
# The secret stays the same for the same API key, so one run is enough. The script never
# prints the secret.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/stripe-common.sh
. scripts/stripe-common.sh

load_stripe_key
[[ -f .env ]] || die ".env does not exist, copy .env.example first"

secret="$(stripe_cli listen --print-secret 2>/dev/null | tr -d '[:space:]')" ||
	die "stripe listen --print-secret failed"
[[ "$secret" =~ ^whsec_[A-Za-z0-9]+$ ]] || die "stripe listen did not return a webhook secret"

umask 077
tmp="$(mktemp .env.XXXXXX)"
trap 'rm -f "$tmp"' EXIT
# The secret goes to awk through the environment, not as an argument, so ps cannot show it.
SECRET="$secret" awk '
	/^STRIPE_WEBHOOK_SECRET=/ { if (!done) print "STRIPE_WEBHOOK_SECRET=" ENVIRON["SECRET"]; done = 1; next }
	{ print }
	END { if (!done) print "STRIPE_WEBHOOK_SECRET=" ENVIRON["SECRET"] }
' .env >"$tmp"
# Write into the same file, so .env keeps its owner and mode.
cat "$tmp" >.env
echo "STRIPE_WEBHOOK_SECRET is set in .env. Restart the server to use it."
