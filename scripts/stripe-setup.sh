#!/usr/bin/env bash
# Creates the Stripe test objects that ToolPort needs. It is safe to run more than once:
# it finds the objects of an earlier run and changes them only when they differ.
#
# For each paid plan: a product with the id toolport_<code> and a recurring price with the
# lookup key of the plan. The server finds the price by that lookup key.
# Also one Customer Portal configuration (metadata toolport=portal): cancel at period end,
# update the payment method, invoice history, and billing address.
#
# Usage: scripts/stripe-setup.sh [PLAN ...]
#   PLAN is code:lookup_key:amount_cents:currency:interval, for example
#   pro:toolport_pro_monthly:900:usd:month
#   Without arguments it uses the paid rows of the plans table in the first migration.
#   Change the arguments when you change a paid row of the plans table.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/stripe-common.sh
. scripts/stripe-common.sh

DEFAULT_PLANS=("pro:toolport_pro_monthly:900:usd:month")

load_stripe_key
command -v jq >/dev/null || die "this script needs jq"

# Calls the Stripe API and prints the JSON. A Stripe error stops the script.
api() {
	local out
	out="$(stripe_cli "$@")" || die "stripe $1 $2 failed"
	if jq -e 'has("error")' <<<"$out" >/dev/null 2>&1; then
		die "stripe $1 $2: $(jq -r '.error.message' <<<"$out")"
	fi
	printf '%s' "$out"
}

# Same as api, but prints nothing when the object does not exist.
api_find() {
	local out
	out="$(stripe_cli get "$1")" || die "stripe get $1 failed"
	if jq -e '.error.code == "resource_missing"' <<<"$out" >/dev/null 2>&1; then
		return 0
	fi
	if jq -e 'has("error")' <<<"$out" >/dev/null 2>&1; then
		die "stripe get $1: $(jq -r '.error.message' <<<"$out")"
	fi
	printf '%s' "$out"
}

setup_plan() {
	local code lookup_key amount currency interval extra
	IFS=: read -r code lookup_key amount currency interval extra <<<"$1"
	[[ -z "${extra:-}" && "$code" =~ ^[a-z0-9_-]+$ && "$lookup_key" =~ ^[a-z0-9_]+$ ]] ||
		die "bad plan '$1', use code:lookup_key:amount_cents:currency:interval"
	[[ "$amount" =~ ^[0-9]+$ && "$amount" -gt 0 ]] || die "bad amount in '$1'"
	[[ "$interval" == month || "$interval" == year ]] || die "bad interval in '$1'"

	local product_id="toolport_$code"
	local name
	name="ToolPort $(tr '[:lower:]' '[:upper:]' <<<"${code:0:1}")${code:1}"
	local product
	product="$(api_find "/products/$product_id")"
	if [[ -z "$product" ]]; then
		api post /products --confirm -d "id=$product_id" -d "name=$name" \
			-d "metadata[toolport_plan]=$code" >/dev/null
		echo "product $product_id: created"
	elif [[ "$(jq -r '.active' <<<"$product")" != true ]]; then
		api post "/products/$product_id" --confirm -d active=true >/dev/null
		echo "product $product_id: active again"
	else
		echo "product $product_id: exists"
	fi

	local prices price
	prices="$(api get /prices -d "lookup_keys[]=$lookup_key" -d active=true)"
	price="$(jq -c '.data[0] // empty' <<<"$prices")"
	if [[ -n "$price" ]] && jq -e --arg p "$product_id" --argjson a "$amount" \
		--arg c "$currency" --arg i "$interval" \
		'.product == $p and .unit_amount == $a and .currency == $c and .recurring.interval == $i' \
		<<<"$price" >/dev/null; then
		echo "price $lookup_key: exists ($(jq -r '.id' <<<"$price"))"
		return 0
	fi
	# A Stripe price cannot change. A new price takes over the lookup key, and the old
	# price is turned off. Existing subscriptions keep the old price.
	local new_id
	new_id="$(api post /prices --confirm -d "product=$product_id" -d "unit_amount=$amount" \
		-d "currency=$currency" -d "recurring[interval]=$interval" \
		-d "lookup_key=$lookup_key" -d transfer_lookup_key=true \
		-d "metadata[toolport_plan]=$code" | jq -r '.id')"
	if [[ -n "$price" ]]; then
		api post "/prices/$(jq -r '.id' <<<"$price")" --confirm -d active=false >/dev/null
		echo "price $lookup_key: replaced by $new_id"
	else
		echo "price $lookup_key: created ($new_id)"
	fi
}

setup_portal() {
	local list id
	list="$(api get /billing_portal/configurations -d active=true -d limit=100)"
	id="$(jq -r '[.data[] | select(.metadata.toolport == "portal")][0].id // empty' <<<"$list")"
	local params=(
		-d "business_profile[headline]=ToolPort subscription"
		-d "features[subscription_cancel][enabled]=true"
		-d "features[subscription_cancel][mode]=at_period_end"
		-d "features[payment_method_update][enabled]=true"
		-d "features[invoice_history][enabled]=true"
		-d "features[customer_update][enabled]=true"
		-d "features[customer_update][allowed_updates][]=address"
		-d "features[subscription_update][enabled]=false"
		-d "metadata[toolport]=portal"
	)
	if [[ -n "$id" ]]; then
		api post "/billing_portal/configurations/$id" --confirm "${params[@]}" >/dev/null
		echo "portal configuration $id: updated"
	else
		id="$(api post /billing_portal/configurations --confirm "${params[@]}" | jq -r '.id')"
		echo "portal configuration $id: created"
	fi
}

plans=("$@")
[[ ${#plans[@]} -gt 0 ]] || plans=("${DEFAULT_PLANS[@]}")
for plan in "${plans[@]}"; do
	setup_plan "$plan"
done
setup_portal
echo "done"
