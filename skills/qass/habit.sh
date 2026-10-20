#!/bin/sh
# The owner's habits: a kind of question they answered with qass's own pick three times in a row,
# which qass then settles itself and tells them in a two-line note. A kind is `<verb>:<repo>`, and
# only reversible work inside a repo can be one: the verbs below. Everything else (commit, push,
# merge, release, deploy, delete, anything external, production, billing, secrets, a design or a
# name) is refused here, so it always stays a card whatever qass counts. qass never edits this
# file by hand; the owner reads it with `habits` and turns one off with `habit off <kind>`.
#
#   habit.sh answered <verb:repo> same|other   count their answer (same as qass's pick, or not)
#   habit.sh may <verb:repo>                   exit 0 when qass may settle it itself
#   habit.sh ls                                one line per kind: count, last answer, off
#   habit.sh off <verb:repo> | on <verb:repo> | clear
set -u
file="@QASS_DATA@/habits"
[ -n "${QASS_HABITS:-}" ] && file="$QASS_HABITS"
need=3

kind() {
  verb=${1%%:*}
  repo=${1#*:}
  case "$verb" in
    edit | test | build | research | script) ;;
    *)
      echo "habit.sh: $verb is not reversible work in a repo: it stays a card" >&2
      exit 2
      ;;
  esac
  if [ "$repo" = "$1" ] || [ -z "$repo" ] || ! printf '%s' "$repo" | grep -Eq '^[a-z0-9][a-z0-9._-]{0,39}$'; then
    echo "habit.sh: a kind is <verb>:<repo>, the repo in lowercase letters, digits, . _ -" >&2
    exit 2
  fi
}

# One line a kind: `<kind> <count> <date> on|off`.
line_of() { [ -f "$file" ] && awk -v k="$1" '$1 == k { print; exit }' "$file"; }

put() {
  tmp="$file.tmp.$$"
  { [ -f "$file" ] && awk -v k="$1" '$1 != k' "$file"; printf '%s %s %s %s\n' "$1" "$2" "$(date +%F)" "$3"; } > "$tmp"
  mv "$tmp" "$file"
}

# One writer at a time: a count read and written back must not lose another's.
lock="$file.lock"
case "${1:-}" in
  answered | off | on | clear)
    n=0
    until mkdir "$lock" 2>/dev/null; do
      # A lock left by a writer that was killed (older than a minute) is taken over.
      [ -n "$(find "$lock" -maxdepth 0 -mmin +1 2>/dev/null)" ] && rmdir "$lock" 2>/dev/null
      n=$((n + 1))
      [ "$n" -gt 100 ] && { echo "habit.sh: $lock is held; try again" >&2; exit 1; }
      sleep 0.05
    done
    trap 'rmdir "$lock"' EXIT
    ;;
esac

case "${1:-}" in
  answered)
    kind "${2:-}"
    old=$(line_of "$2")
    count=$(printf '%s' "$old" | awk '{print $2}')
    state=$(printf '%s' "$old" | awk '{print $4}')
    case "${3:-}" in
      same) count=$(( ${count:-0} + 1 )) ;;
      other) count=0 ;;
      *) echo "habit.sh answered <verb:repo> same|other" >&2; exit 2 ;;
    esac
    put "$2" "$count" "${state:-on}"
    ;;
  may)
    kind "${2:-}"
    set -- $(line_of "$2")
    [ "${2:-0}" -ge "$need" ] && [ "${4:-on}" = on ]
    ;;
  ls)
    [ -s "$file" ] || { echo "no habits yet"; exit 0; }
    awk -v need="$need" '{ printf "%s: %s of %s same answers, last %s%s\n", $1, $2, need, $3, ($4 == "off" ? ", off" : "") }' "$file"
    ;;
  off | on)
    kind "${2:-}"
    old=$(line_of "$2")
    count=$(printf '%s' "$old" | awk '{print $2}')
    put "$2" "${count:-0}" "$1"
    ;;
  clear)
    rm -f "${file:?}"
    ;;
  *)
    echo "habit.sh answered <verb:repo> same|other | may <verb:repo> | ls | off|on <verb:repo> | clear" >&2
    exit 2
    ;;
esac
