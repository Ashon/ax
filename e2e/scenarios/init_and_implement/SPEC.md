# init_and_implement (full-lifecycle completion contract)

Exercises the full ax lifecycle — `ax init` → `ax up` → orchestrator
turn — against a real codex team. Validates the completion-contract
enforcement added by the `MissingCompletionEvidence` / startup
recovery / periodic reconciler work by checking that the persisted
task state carries the contract marker after the run.

## Goal

Starting from a project that has no `.ax/` directory, the scenario:

1. Generates a role-axis config via `ax init --axis role --codex`.
2. Brings the project up with `ax up`.
3. Sends the user prompt to a fresh root orchestrator session.
4. The orchestrator delegates to the `greeter` workspace via
   `start_task`.
5. `greeter` creates `greeter/hello.sh` (executable, `echo hello, ax`)
   and marks its task completed **with** the Completion Reporting
   Contract marker in `result`.

## Ownership

- `greeter` owns the `greeter/` subdirectory.
- The orchestrator must not write files itself; delegation is the
  whole point.

## Validation (`validate.sh`)

- `greeter/hello.sh` exists, is executable, prints exactly
  `hello, ax`.
- The daemon's persisted task state (`tasks-state.json`, sibling of
  the Unix socket) contains the `remaining owned dirty files=`
  marker — proof that the worker's completion went through the
  contract-enforced path instead of being rejected.
