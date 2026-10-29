# Sourced by the git hooks. Succeeds when git was run from a Claude Code session: either the
# session's CLAUDECODE marker is set, or a Claude Code process is an ancestor (which survives
# commands that clear the environment, e.g. `env -i`). Fails closed if the chain can't be read.
is_claude() {
  [ -n "${CLAUDECODE:-}" ] && return 0
  # Absolute path so a `ps` planted earlier on PATH can't fake the answer.
  ps_bin=/bin/ps
  [ -x "$ps_bin" ] || ps_bin=/usr/bin/ps
  [ -x "$ps_bin" ] || return 0

  pid=$$
  while [ "$pid" -gt 1 ]; do
    args=$("$ps_bin" -o args= -p "$pid" 2>/dev/null) || return 0
    # Native binary (…/claude …) or npm install (node …/@anthropic-ai/claude-code/…).
    set -- $args
    case ${1:-} in claude | */claude) return 0 ;; esac
    case $args in *@anthropic-ai/claude-code*) return 0 ;; esac

    set -- $("$ps_bin" -o ppid= -p "$pid" 2>/dev/null)
    pid=${1:-}
    case $pid in '' | *[!0-9]*) return 0 ;; esac
  done
  return 1
}
