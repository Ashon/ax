#!/bin/sh
# Expects the auto axis to resolve to domain (or hybrid) on a
# fixture that's obviously split by business domain.
set -e

cfg=".ax/config.yaml"
if [ ! -f "$cfg" ]; then
    echo "missing $cfg" >&2
    exit 1
fi

if ! grep -qE '^#\s*axis:\s*(domain|hybrid)\b' "$cfg"; then
    echo "expected '# axis: domain' or '# axis: hybrid' in $cfg, got:" >&2
    grep -E '^#\s*axis:' "$cfg" >&2 || echo "(no axis comment)" >&2
    exit 1
fi

# At least one of the fixture's domains (or a close variant) should
# appear as a workspace name. Agents sometimes pluralise / pick a
# near-synonym, so include common variants.
if ! grep -qiE '^\s*(users?|accounts?|auth|orders?|checkout|billing|payments?|inventory|stock|catalog|products?):' "$cfg"; then
    echo "expected at least one domain-style workspace in $cfg" >&2
    exit 1
fi
exit 0
