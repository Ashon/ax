#!/bin/sh
# Expects the auto axis to resolve to role (or hybrid) on a fixture
# that's obviously split by implementation layer. The setup agent's
# config.yaml must carry an `# axis:` comment and at least one
# recognisable role-style workspace.
set -e

cfg=".ax/config.yaml"
if [ ! -f "$cfg" ]; then
    echo "missing $cfg" >&2
    exit 1
fi

if ! grep -qE '^#\s*axis:\s*(role|hybrid)\b' "$cfg"; then
    echo "expected '# axis: role' or '# axis: hybrid' in $cfg, got:" >&2
    grep -E '^#\s*axis:' "$cfg" >&2 || echo "(no axis comment)" >&2
    exit 1
fi

if ! grep -qiE '^\s*(frontend|backend|infra|api|ui|web|server|devops|platform|docs|qa):' "$cfg"; then
    echo "expected at least one role-style workspace in $cfg" >&2
    exit 1
fi
exit 0
