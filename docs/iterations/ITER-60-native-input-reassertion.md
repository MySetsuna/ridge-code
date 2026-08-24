# Iteration 60 — native Windows input mode reassertion

## Decision

The TUI reader reasserts native Windows input mode before and after every
Crossterm `event::poll`/`event::read` cycle. `ENABLE_VIRTUAL_TERMINAL_INPUT`
stays disabled on native console handles, so VT escape tails such as
`[C`, `[D`, and `[3~` cannot be emitted as literal key input after Crossterm
rewrites the console mode while polling.

ConPTY and redirected stdin are unchanged: `GetConsoleMode` fails for those
pipe handles, so the native guard is a no-op and Crossterm remains the parser.
On Unix, the dependency enables Crossterm's `use-dev-tty` raw descriptor
poll/select backend; a hard-exclusive custom reader remains an explicit gap.

## Evidence contract

- Unit suite must pass after the reader change.
- Windows ConPTY `InputFixture` and `BusyFixture` must pass repeatedly.
- Current local evidence: workspace tests `213 + 464 + 6 + 1 + 9 + 4 + 56 + 27 (+1 ignored) + doctest`, `cargo llvm-cov` 83.40% lines / 82.71% regions, and bounded soak `10 × 3` with zero timeouts.
- `npm run spectree:check`, `stc validate`, and `stc status` must agree with
  `specs/agent-input.md` and show no stale targets.

## Remaining gap

Native Windows console and macOS/Linux PTY matrices still need physical-host
acceptance. Until then, the project makes no cross-terminal completeness claim.
