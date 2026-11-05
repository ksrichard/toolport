# shellcheck shell=bash
# Shared parts of the Stripe scripts. Source it, do not run it.
# It reads STRIPE_SECRET_KEY from the environment or from .env, refuses live keys, and
# exports it as STRIPE_API_KEY. The stripe-cli container reads that variable, so the key is
# never on a command line and never in the output.

STRIPE_CLI_IMAGE="stripe/stripe-cli:v1.54.1"

die() {
	echo "error: $*" >&2
	exit 1
}

# Prints the value of one variable in .env. It does not run .env as a script.
env_value() {
	[[ -f .env ]] || return 0
	grep -E "^$1=" .env | tail -n 1 | cut -d= -f2- | sed -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/"
}

load_stripe_key() {
	local key="${STRIPE_SECRET_KEY:-}"
	[[ -n "$key" ]] || key="$(env_value STRIPE_SECRET_KEY)"
	case "$key" in
	sk_live_* | rk_live_*) die "this is a live Stripe key. Use a test key (sk_test_ or rk_test_)." ;;
	sk_test_* | rk_test_*) ;;
	"") die "set STRIPE_SECRET_KEY in .env" ;;
	*) die "STRIPE_SECRET_KEY is not a Stripe test key (sk_test_ or rk_test_)" ;;
	esac
	export STRIPE_API_KEY="$key"
	command -v docker >/dev/null || die "this script needs docker"
}

stripe_cli() {
	docker run --rm -e STRIPE_API_KEY "$STRIPE_CLI_IMAGE" "$@"
}
