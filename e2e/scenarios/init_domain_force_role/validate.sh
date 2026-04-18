#!/bin/sh
# --axis role was forced on a domain-shaped fixture. The agent
# must override the observed boundary and produce a role-centric
# config; if the axis comment still says 'domain' or 'hybrid' the
# override was ignored.
set -e

cfg=".ax/config.yaml"
if [ ! -f "$cfg" ]; then
    echo "missing $cfg" >&2
    exit 1
fi

if ! grep -qE '^#\s*axis:\s*role\b' "$cfg"; then
    echo "expected '# axis: role' (override ignored) in $cfg, got:" >&2
    grep -E '^#\s*axis:' "$cfg" >&2 || echo "(no axis comment)" >&2
    exit 1
fi

if ! grep -qiE '^\s*(frontend|backend|infra|api|ui|web|server|devops|platform|docs|qa|cli):' "$cfg"; then
    echo "expected at least one role-style workspace in $cfg" >&2
    exit 1
fi
exit 0
