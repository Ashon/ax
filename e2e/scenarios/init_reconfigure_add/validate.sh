#!/bin/sh
# After ax init --reconfigure on this fixture, the agent should:
#  1. preserve the role axis,
#  2. append a `# reconfigured:` trail comment near the top,
#  3. add a workspace covering the new infra/ directory.
set -e

cfg=".ax/config.yaml"
if [ ! -f "$cfg" ]; then
    echo "missing $cfg" >&2
    exit 1
fi

if ! grep -qE '^#\s*axis:\s*role\b' "$cfg"; then
    echo "axis was not preserved as role in $cfg:" >&2
    grep -E '^#\s*axis:' "$cfg" >&2 || echo "(no axis comment)" >&2
    exit 1
fi

if ! grep -qE '^#\s*reconfigured:' "$cfg"; then
    echo "expected a '# reconfigured:' trail comment in $cfg" >&2
    exit 1
fi

# At least one workspace name that could plausibly cover infra/ —
# agents vary on exact naming.
if ! grep -qiE '^\s*(infra|infrastructure|terraform|platform|devops|cloud):' "$cfg"; then
    echo "expected a new workspace for the infra/ directory in $cfg" >&2
    exit 1
fi
exit 0
