#!/bin/sh
# Poll a mint's /health and report or restart it when it stays unhealthy.
#
#   sp-health-monitor.sh [--config FILE] [--once]
#
# Settings come from three places, strongest first: an environment variable, a
# line KEY=VALUE in the config file (--config FILE, or HEALTH_CONFIG), and the
# default. The settings are:
#
#   HEALTH_URL            the mint's /health, e.g. http://127.0.0.1:3100/health (required)
#   HEALTH_NAME           a name for the log lines (default: the URL)
#   HEALTH_INTERVAL_SECS  seconds between polls (default 30)
#   HEALTH_TIMEOUT_SECS   seconds a poll may take before it counts as a failure (default 5)
#   HEALTH_FAILURES       failures in a row before the action is taken (default 3)
#   HEALTH_GRACE_SECS     seconds to leave the mint alone after a restart (default 60)
#   HEALTH_ACTION         report or restart (default report)
#   HEALTH_RESTART_CMD    the command that restarts the mint (required for restart)
#   HEALTH_REPORT_CMD     an optional command run when the mint is unhealthy and again
#                         when it is back; the text is in $HEALTH_MESSAGE
#
# Healthy is HTTP 200. Anything else (503, no answer, a timeout) is a failure.
# With --once the script polls one time, prints the result and exits 0 (healthy)
# or 1, and takes no action. It stops on SIGTERM or SIGINT.

set -u

config=${HEALTH_CONFIG:-}
once=no
while [ $# -gt 0 ]; do
    case $1 in
        --config)
            [ $# -ge 2 ] || { echo "--config needs a file" >&2; exit 2; }
            config=$2
            shift 2
            ;;
        --once)
            once=yes
            shift
            ;;
        -h | --help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument $1" >&2
            exit 2
            ;;
    esac
done

# Read KEY=VALUE lines. A setting already in the environment is left alone, so
# the environment wins over the file.
if [ -n "$config" ]; then
    [ -f "$config" ] || { echo "the config file $config does not exist" >&2; exit 2; }
    while IFS= read -r line || [ -n "$line" ]; do
        case $line in
            '' | '#'*) continue ;;
        esac
        case $line in
            *=*) ;;
            *) echo "$config: not KEY=VALUE: $line" >&2; exit 2 ;;
        esac
        key=${line%%=*}
        value=${line#*=}
        case $key in
            HEALTH_URL | HEALTH_NAME | HEALTH_INTERVAL_SECS | HEALTH_TIMEOUT_SECS | \
                HEALTH_FAILURES | HEALTH_GRACE_SECS | HEALTH_ACTION | HEALTH_RESTART_CMD | \
                HEALTH_REPORT_CMD) ;;
            *) echo "$config: unknown setting $key" >&2; exit 2 ;;
        esac
        case $value in
            \"*\") value=${value#\"}; value=${value%\"} ;;
        esac
        eval "already=\${$key+set}"
        [ -n "$already" ] || eval "$key=\$value"
    done < "$config"
fi

: "${HEALTH_INTERVAL_SECS:=30}"
: "${HEALTH_TIMEOUT_SECS:=5}"
: "${HEALTH_FAILURES:=3}"
: "${HEALTH_GRACE_SECS:=60}"
: "${HEALTH_ACTION:=report}"
: "${HEALTH_RESTART_CMD:=}"
: "${HEALTH_REPORT_CMD:=}"
: "${HEALTH_URL:=}"
: "${HEALTH_NAME:=$HEALTH_URL}"

[ -n "$HEALTH_URL" ] || { echo "HEALTH_URL is required" >&2; exit 2; }
for name in HEALTH_INTERVAL_SECS HEALTH_TIMEOUT_SECS HEALTH_FAILURES HEALTH_GRACE_SECS; do
    eval "v=\$$name"
    case $v in
        '' | *[!0-9]*) echo "$name must be a whole number, not '$v'" >&2; exit 2 ;;
    esac
done
for name in HEALTH_INTERVAL_SECS HEALTH_TIMEOUT_SECS HEALTH_FAILURES; do
    eval "v=\$$name"
    [ "$v" -ge 1 ] || { echo "$name must be at least 1" >&2; exit 2; }
done
case $HEALTH_ACTION in
    report) ;;
    restart)
        [ -n "$HEALTH_RESTART_CMD" ] || { echo "HEALTH_ACTION=restart needs HEALTH_RESTART_CMD" >&2; exit 2; }
        ;;
    *) echo "HEALTH_ACTION must be report or restart, not '$HEALTH_ACTION'" >&2; exit 2 ;;
esac

log() {
    printf '%s %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$HEALTH_NAME" "$*"
}

report() {
    log "$1"
    if [ -n "$HEALTH_REPORT_CMD" ]; then
        HEALTH_MESSAGE="$HEALTH_NAME: $1" sh -c "$HEALTH_REPORT_CMD" ||
            log "the report command failed"
    fi
}

# Prints the HTTP status, or 000 when there was no answer.
poll() {
    code=$(curl -sS -m "$HEALTH_TIMEOUT_SECS" -o /dev/null -w '%{http_code}' "$HEALTH_URL" 2>/dev/null) ||
        code=000
    printf '%s' "$code"
}

if [ "$once" = yes ]; then
    code=$(poll)
    if [ "$code" = 200 ]; then
        log "healthy (200)"
        exit 0
    fi
    log "unhealthy ($code)"
    exit 1
fi

# Sleep in the background and wait for it, so that a signal ends the wait at once.
stop=no
trap 'stop=yes; [ -z "${sleeper:-}" ] || kill "$sleeper" 2>/dev/null' TERM INT
nap() {
    sleep "$1" &
    sleeper=$!
    wait "$sleeper" 2>/dev/null
    sleeper=
}

log "watching $HEALTH_URL every ${HEALTH_INTERVAL_SECS}s; $HEALTH_ACTION after $HEALTH_FAILURES failures in a row"
failures=0
alarmed=no
while [ "$stop" = no ]; do
    code=$(poll)
    if [ "$code" = 200 ]; then
        if [ "$alarmed" = yes ]; then
            report "healthy again"
            alarmed=no
        fi
        failures=0
    else
        failures=$((failures + 1))
        log "unhealthy ($code), $failures in a row"
        if [ "$failures" -ge "$HEALTH_FAILURES" ]; then
            report "unhealthy for $failures polls in a row (last answer $code)"
            alarmed=yes
            if [ "$HEALTH_ACTION" = restart ]; then
                log "restarting"
                if sh -c "$HEALTH_RESTART_CMD"; then
                    log "restart command done; leaving it alone for ${HEALTH_GRACE_SECS}s"
                else
                    log "the restart command failed"
                fi
                failures=0
                [ "$stop" = no ] && nap "$HEALTH_GRACE_SECS"
                continue
            fi
        fi
    fi
    [ "$stop" = no ] && nap "$HEALTH_INTERVAL_SECS"
done
log "stopped"
exit 0
