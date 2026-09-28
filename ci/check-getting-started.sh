#!/usr/bin/env bash
#
# Execute the command transcript in a guide — docs/getting-started.md unless
# another is named as the first argument — so the guide can never drift from
# what the CLI actually does.
#
# How it works. The guide marks each runnable ```console block with an HTML
# comment on the line directly above it:
#
#   <!-- exec -->              every `$ ` command in the block must exit 0
#   <!-- exec allow-fail -->   commands may exit non-zero (error/finding demos)
#   <!-- exec expect -->       every command must exit 0 AND print exactly the
#                              lines that follow it, up to the next `$ ` line
#
# A command ending in a heredoc (`<<'EOF'`) takes the lines after it, up to the
# delimiter, as its input — which is how a guide writes a file a reader can see.
#
# All the marked blocks are run, top to bottom, as ONE shell session in a
# throwaway sandbox: `cd` and shell variables persist across blocks, exactly as a
# reader following along would experience. Only `$ `-prefixed lines are executed;
# the expected-output lines are ignored (IDs are random and paths are absolute,
# so matching them verbatim would be noise) except in an `expect` block, whose
# commands must be deterministic. A `$` command that exits non-zero in a strict
# block fails the run and prints the offending command. An `expect` command runs
# in a subshell to capture its output, so a `cd` there would not persist.
#
# The `prov` name in the transcript resolves to the built binary (below), and
# PROV_QUIET silences the config-advisory line so it can't interleave with
# asserted output.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GUIDE="${1:-$ROOT/docs/getting-started.md}"
[[ "$GUIDE" == /* ]] || GUIDE="$ROOT/$GUIDE"
NAME="$(basename "$GUIDE" .md)"

# Locate the binary: an explicit PROV_BIN wins; otherwise prefer a release
# build, then a debug build, then build a debug one on the spot.
if [[ -n "${PROV_BIN:-}" ]]; then
  BIN="$PROV_BIN"
elif [[ -x "$ROOT/target/release/prov" ]]; then
  BIN="$ROOT/target/release/prov"
elif [[ -x "$ROOT/target/debug/prov" ]]; then
  BIN="$ROOT/target/debug/prov"
else
  echo "building prov (debug) ..."
  ( cd "$ROOT" && cargo build -p prov-cli ) || exit 1
  BIN="$ROOT/target/debug/prov"
fi
echo "using binary: $BIN"

# The transcript writes `prov ...`; route it to the built binary. Defined as
# a function so command substitution (`$(prov id …)`) inherits it too.
prov() { "$BIN" "$@"; }
export PROV_QUIET=1

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
cd "$SANDBOX"

ran=0
failed=0
mode="none"     # none | strict | allowfail | expect  (the block currently open)
pending=""      # a marker seen, waiting for the block it applies to
inblock=0
cmd=""          # the command read but not yet run
heredoc=""      # the delimiter a heredoc command is still reading up to
expected=""     # an `expect` command's output, as the guide shows it
have_cmd=0

# Run the command read so far, in the block's mode.
flush() {
  (( have_cmd )) || return 0
  have_cmd=0
  ran=$(( ran + 1 ))
  if [[ "$mode" == "expect" ]]; then
    local out rc
    out="$(eval "$cmd" 2>/dev/null)"; rc=$?
    if (( rc != 0 )); then
      echo "TRANSCRIPT FAILED (exit $rc): $cmd" >&2
      failed=$(( failed + 1 ))
    elif [[ "$out" != "$(printf '%s' "$expected")" ]]; then
      echo "TRANSCRIPT OUTPUT DIFFERS: $cmd" >&2
      diff <(printf '%s\n' "$expected") <(printf '%s\n' "$out") >&2
      failed=$(( failed + 1 ))
    fi
    return 0
  fi
  if eval "$cmd"; then
    :
  else
    local rc=$?
    if [[ "$mode" == "strict" ]]; then
      echo "TRANSCRIPT FAILED (exit $rc): $cmd" >&2
      failed=$(( failed + 1 ))
    fi
  fi
}

while IFS= read -r line; do
  # Inside a heredoc every line, fences included, is the command's input.
  if [[ -n "$heredoc" ]]; then
    cmd+=$'\n'"$line"
    [[ "$line" == "$heredoc" ]] && heredoc=""
    continue
  fi

  # Marker lines arm the *next* console block.
  if [[ "$line" == '<!-- exec -->' ]]; then pending="strict"; continue; fi
  if [[ "$line" == '<!-- exec allow-fail -->' ]]; then pending="allowfail"; continue; fi
  if [[ "$line" == '<!-- exec expect -->' ]]; then pending="expect"; continue; fi

  # Fence toggles a block. An opening fence consumes any pending marker.
  if [[ "$line" == '```'* ]]; then
    if (( inblock )); then
      flush
      inblock=0; mode="none"
    elif [[ -n "$pending" ]]; then
      inblock=1; mode="$pending"; pending=""
    fi
    continue
  fi

  # A non-blank, non-fence line before a block clears a stray marker.
  if [[ -n "$pending" && -n "${line// }" ]]; then pending=""; fi

  (( inblock )) || continue
  [[ "$mode" == "none" ]] && continue
  if [[ "$line" != '$ '* ]]; then
    # Expected output: checked in an `expect` block, ignored elsewhere.
    if (( have_cmd )) && [[ "$mode" == "expect" ]]; then
      if [[ -z "$expected" ]]; then expected="$line"; else expected+=$'\n'"$line"; fi
    fi
    continue
  fi

  flush
  cmd="${line#'$ '}"
  expected=""
  have_cmd=1
  if [[ "$cmd" =~ \<\<-?\'?([A-Za-z_]+)\'?[[:space:]]*$ ]]; then
    heredoc="${BASH_REMATCH[1]}"
  fi
done < "$GUIDE"
flush

if (( failed )); then
  echo "$NAME transcript: $failed command(s) failed of $ran run" >&2
  exit 1
fi
echo "$NAME transcript: all $ran command(s) ran clean"
