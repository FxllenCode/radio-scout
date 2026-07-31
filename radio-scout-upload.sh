#!/bin/sh
# Radio-Scout uploadScript for Trunk Recorder (#43, spec US 6).
#
#   "uploadScript": "/opt/radio-scout-upload.sh --env-file /etc/radio-scout.env"
#
# Trunk Recorder runs this after every finished call and appends three paths of
# its own: the rendered `.wav`, the call `.json`, and the `.m4a`
# (`call_concluder.cc`'s `run_upload_script_argv`). This posts that pair to
# Radio-Scout's Trunk-Recorder-native endpoint, which reads the whole of what TR
# writes — the emergency and encrypted flags, priority, audio type, the precise
# duration, per-frequency decode health, and the aliases radios put over the air.
# The rdio-scanner dialect the `rdioscanner_uploader` plugin speaks carries none
# of that, which is the reason this script exists.
#
# POSIX sh, no bashisms: this lands on whatever the recorder happens to be.

set -eu

ENDPOINT="/api/trunk-recorder-call-upload"

# Set only by `--server`. Resolved against the environment further down, once
# any `--env-file` has been read — so the precedence is one expression rather
# than an assignment whose ordering has to be remembered.
CLI_SERVER=""
ENV_FILE=""

usage() {
	cat <<EOF
Post a finished Trunk Recorder call to Radio-Scout.

Usage, from Trunk Recorder's uploadScript hook:
  radio-scout-upload.sh [options] <audio.wav> <call.json> <audio.m4a>

Trunk Recorder appends those three paths itself; you write only the options.

Options:
  --server <url>     Radio-Scout's base URL, e.g. http://scout.lan:3000.
                     Defaults to \$RADIO_SCOUT_URL.
  --env-file <path>  Read RADIO_SCOUT_URL / RADIO_SCOUT_API_KEY from a file.
                     It is **sourced by the shell**, so it is \`KEY=value\` lines
                     in shell syntax — quote a value with spaces. Keep it mode
                     0600 and readable by the user Trunk Recorder runs as; it
                     holds the key.
  -h, --help         This.

The API key comes from \$RADIO_SCOUT_API_KEY or --env-file, and deliberately
has no flag of its own: a command line is world-readable through ps.
EOF
}

# Everything this script says goes to stderr, which Trunk Recorder inherits, so
# an operator finds it in the recorder's own output beside the call it was
# about. Prefixed, because that log has a lot else in it.
say() { printf 'radio-scout: %s\n' "$*" >&2; }

# The install is wrong in a way no retry can fix — a missing argument, a file
# that is not there. Exit non-zero so Trunk Recorder logs "Upload script failed
# with status N" and the operator finds out on the very first call.
#
# Nothing about the network or the server ever comes through here. A non-zero
# exit makes TR skip `plugman_call_end` for the call entirely
# (`call_concluder.cc:981-987`), so every *other* plugin — including an
# rdioscanner_uploader feeding a production rdio-scanner — never runs either.
# A Radio-Scout outage must not take another uploader down with it.
die() {
	say "$*"
	exit 2
}

# Handled before anything else, so a human checking their setup by hand gets
# usage rather than a complaint about the three paths they did not pass.
case "${1:-}" in
-h | --help)
	usage
	exit 0
	;;
esac

# Walk the operator's own options and stop at the first thing that is not one.
#
# Counting instead — "everything past the last three arguments is mine" — would
# be wrong the day Trunk Recorder appends a fourth path, and wrong in the worst
# way: the script would read that path as an option, not recognise it, and exit
# non-zero on every single call.
while [ $# -gt 0 ]; do
	case "$1" in
	--server)
		CLI_SERVER="${2:-}"
		[ -n "$CLI_SERVER" ] || die "--server needs a URL"
		shift 2
		;;
	--env-file)
		ENV_FILE="${2:-}"
		[ -n "$ENV_FILE" ] || die "--env-file needs a path"
		shift 2
		;;
	--) # everything after this is Trunk Recorder's, whatever it looks like
		shift
		break
		;;
	-*) die "unknown option: $1 (Trunk Recorder appends the file paths itself)" ;;
	*) break ;; # the first of Trunk Recorder's paths
	esac
done

# `-ge`, not `-eq`: a Trunk Recorder that appends a fourth path is a Trunk
# Recorder we still upload for. Its argument list has grown before and can
# again, and exiting non-zero over an *extra* one would take every other plugin
# on the recorder down with it, on every call, until somebody worked out why.
# `TrMeta` in `src/ingest.rs` takes the same stance for the same reason.
[ $# -ge 3 ] || die "expected <audio.wav> <call.json> <audio.m4a> from Trunk Recorder, got $# argument(s)"

WAV="$1"
JSON="$2"
M4A="$3"

if [ -n "$ENV_FILE" ]; then
	[ -r "$ENV_FILE" ] || die "cannot read --env-file $ENV_FILE"
	# `.` rather than parsing it ourselves: the file is the operator's, in the
	# `KEY=value` shape every service manager already writes. Note this is a
	# *source*, not a parse — the file is shell, so a value with spaces needs
	# quoting where systemd's `EnvironmentFile=` would not need it, and anything
	# else in the file runs. `--help` says so; so does the recorders guide. It
	# is the operator's own root-owned file either way, and sourcing is what
	# makes one file work for both.
	# shellcheck disable=SC1090 # the path is the operator's, by definition
	. "$ENV_FILE"
fi

# Flag beats environment beats file — and the file has already been folded into
# the environment above, so this is the whole of the precedence. Resolved *here*
# rather than at the top, because a value read before the file was sourced is a
# value the file cannot set, which is exactly the bug this shape removes.
SERVER="${CLI_SERVER:-${RADIO_SCOUT_URL:-}}"
KEY="${RADIO_SCOUT_API_KEY:-}"

[ -n "$SERVER" ] || die "no server: pass --server, or set RADIO_SCOUT_URL"
[ -n "$KEY" ] || die "no API key: set RADIO_SCOUT_API_KEY, or point --env-file at a file that does"
[ -r "$JSON" ] || die "no call metadata at $JSON"

# Prefer the compressed copy when Trunk Recorder made one. TR appends the `.m4a`
# path whether or not `compressWav` produced the file, so its existence is the
# only thing that says which to send — and 32 kbps AAC against 8 kHz 16-bit PCM
# is most of a home uplink's headroom. Radio-Scout reads either.
if [ -s "$M4A" ]; then
	AUDIO="$M4A"
	AUDIO_TYPE="audio/mp4"
elif [ -s "$WAV" ]; then
	AUDIO="$WAV"
	# `audio/wav`, not the older `audio/x-wav`: it is what the plugin sends, what
	# the enhancement pipeline writes when it re-encodes, and the spelling
	# browsers expect. Radio-Scout accepts every variant, but this string is
	# stored and handed back as the Content-Type, so the two shipped uploaders
	# giving one Call two different types was a difference with no reason.
	AUDIO_TYPE="audio/wav"
else
	die "no audio to upload: neither $M4A nor $WAV"
fi

# The key goes in on **stdin**, as a curl config file, and not as an argument.
# `ps` is world-readable: a secret in any process's command line is a secret
# every user on the recorder can read, and keeping it off *this* script's
# arguments only to hand it to `curl -F key=…` would move the leak one process
# along and call it fixed. Everything else can be an argument; only this cannot.
#
# `-K -` reads options from stdin. `form-string`, not `form`, and the value
# unquoted: a key is opaque bytes an operator chose, and `-F name=value` reads a
# leading `@` as "upload this file" and a leading `<` as "read the value from
# this file", while a quoted config value would take a `\` or a `"` inside the
# key as an escape. `--form-string` interprets none of it, and an unquoted
# config value runs to the end of the line.
#
# Deliberately **not** `--fail-with-body`, which is curl 7.76 (2021). Raspberry
# Pi OS Bullseye ships 7.74, and there an unknown option makes curl exit before
# sending anything — so the newest thing used here is `-w`, which has been in
# curl since the 1990s. The body goes to a file and the status code comes back
# on stdout, which is the portable spelling of the same idea.
BODY_FILE=$(mktemp) || die "cannot create a temporary file"
trap 'rm -f "$BODY_FILE"' EXIT INT TERM

HTTP_STATUS=$(
	printf 'form-string = key=%s\n' "$KEY" |
		curl -sS -K - \
			--max-time 30 \
			-o "$BODY_FILE" \
			-w '%{http_code}' \
			-F "meta=<$JSON" \
			-F "audio=@$AUDIO;type=$AUDIO_TYPE" \
			"${SERVER%/}$ENDPOINT" 2>&1
) && CURL_STATUS=0 || CURL_STATUS=$?

# Everything below is loud and then exits **0**, deliberately — see `die` above.
# The Call is lost to Radio-Scout either way, because Trunk Recorder deletes its
# files after this returns whatever happens; the only thing a non-zero exit
# would buy is taking every other plugin on the recorder down with us.
if [ "$CURL_STATUS" -ne 0 ]; then
	# curl never got an answer: refused, timed out, DNS. `$HTTP_STATUS` holds
	# curl's own message in this case, since its stderr was folded into it.
	say "upload failed (curl $CURL_STATUS): $(printf '%s' "$HTTP_STATUS" | tr '\n' ' ')"
	exit 0
fi

case "$HTTP_STATUS" in
2??) ;;
*)
	# Radio-Scout answered and said no. Its own words are the useful part —
	# "Invalid API key for system 0 talkgroup 54155.", "Incomplete call data:
	# no talkgroup" — so they go in the line rather than just the number.
	say "upload refused (HTTP $HTTP_STATUS): $(tr '\n' ' ' <"$BODY_FILE")"
	exit 0
	;;
esac

exit 0
