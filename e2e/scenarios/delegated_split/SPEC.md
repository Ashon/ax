# delegated_split (L2)

Exercises the orchestrator → worker delegation path by fanning out
a pair of independent tasks to two workspaces.

## Goal

The root orchestrator dispatches two parallel tasks via `start_task`:

- `alpha` writes `alpha/result.txt` containing exactly `alpha`.
- `beta` writes `beta/result.txt` containing exactly `beta`.

## Ownership

- `alpha` owns the `alpha/` subdirectory.
- `beta` owns the `beta/` subdirectory.
- The orchestrator must not write the files itself — delegating is
  the whole point of the scenario.

## Validation

`validate.sh` checks both files exist with their expected contents.
