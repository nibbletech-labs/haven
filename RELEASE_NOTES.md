## v0.1.8: Agents that read the code, and a CLI that stays quiet

Two weeks of agent transcripts drove this release. Haven's CLI now stays silent on success so agents stop discarding its error output, accepts the flags and verbs agents guess, and lets an item be completed more than once. The planning skills gain the discipline a native plan mode imposes, and the orchestrator's spawn transport is fixed to the one that actually delivers results.

**Quiet CLI, forgiving surface**

- **Stderr is silent on success.** Agents were adding `2>/dev/null` to dodge the per-op telemetry line and then never saw the error envelope, in 43% of calls. Telemetry now appends to `$HAVEN_HOME/telemetry.jsonl` by default. `HAVEN_TELEMETRY=stderr` restores the old line, `HAVEN_TELEMETRY=off` drops it. Truncation notes for `list` and `graph` print only on a terminal. MCP telemetry is unchanged.
- **The flags and verbs agents guess now work.** Hidden aliases mined from transcripts: global `--json` and `--format`, `item add --title` and a repeatable `--depends-on`, `--owner` on add and claim, `--by` for `--actor`, `--name` for `--path` on artifact ops, `--reason` wherever `--rationale` is, `artifact get REF [ROLE]`, and `item create`. Unknown top-level verbs such as `edge`, `claim`, or `set-extref` get a did-you-mean.
- **Complete never collides.** `item complete` picks the next free `delivery-N.md` against both the artifact rows and the directory on disk, so a reopened item can be completed again and prior evidence stays as history.
- **Handoff waits can be cleared.** `handoff --wait none` clears the wait on both CLI and MCP, overriding the to-human default.

**Planning and orchestration skills**

- **Plan agents read before they propose.** A new shared reference, `code-planning-method.md`, carries the discipline a native plan mode injects: trace the code path from entry point to effect, find the similar feature, hunt for reuse before writing new code, read the tests, weigh approaches, sequence, name risks. `plan-item` applies it at spec altitude, and `orchestrate-run` forwards it verbatim into every build-plan brief, since a spawned agent inherits no skill. The plan-gate validator now rejects a plan that shows no trace or rewrites code that already exists.
- **Plain subagents by default.** 58% of named teammates went idle without ever reporting, and none were woken by a finished background job. One-shot reporters (plan agent, validator, verifier, reviewer) now spawn as plain subagents whose final message is the result. A named teammate carries a verbatim contract: load SendMessage first, report through it, never end a turn with a job in flight.
- **Liveness is foreground-only.** The executor no longer offers background execution as an alternative, and the watchdog checks the log tail.
- **The post-run audit stops filing into other people's backlogs.** Deltas about the skill itself go to the project that owns the skill's source, repo traps go to that repo's CLAUDE.md, and only a genuine product gap becomes a floating item in the run's project.

**Upgrade notes**

- No database migration is required.
- Rerun `haven skill install` so both the Claude Code and Codex skill sets pick up the new reference and the orchestrate-run transport rules.
- If you relied on the telemetry line on stderr, set `HAVEN_TELEMETRY=stderr`.
