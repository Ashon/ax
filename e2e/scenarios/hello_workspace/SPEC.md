# hello_workspace (L1)

Smallest possible orchestration smoke test.

## Goal

The root orchestrator delegates a trivial task to the single `worker`
workspace. The worker creates `worker/hello.txt` containing the
exact line `hello, world`.

## Ownership

- `worker` owns the `worker/` subdirectory.

## Validation

`validate.sh` at the project root checks that the file exists and
contains the expected line.
