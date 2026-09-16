# The Ledger corpus

**Nothing in this directory was recorded from any real program.** Every byte of it
was written by hand for one purpose: to prove that Rimaia's provider seam
(ADR-0026) is cut in the right place. It is evidence about **Rimaia** and nothing
else — not about any agent CLI, vendor or product, none of which is named here or
implied by anything below.

"Ledger" is a provider that does not exist. It is deliberately not named after a
real product, because a name would invite a later reader to treat these files as
observations about that product. They are not observations at all.

## Why it is a sibling of `cli/`, and not a file in it

`cli/` is a corpus of **recordings**. Seven tests iterate it asserting properties
every real recording has — a top-level `type`, a terminal `result` event carrying
`terminal_reason` and `subtype`. A foreign file dropped in beside them breaks all
seven, and every repair is an exclusion list: exactly the carve-out that erodes
what the corpus can claim. So this directory has its own structural tests in
`tests/harness_ledger.rs`, and `tests/harness.rs`'s assertions about `cli/` were
not loosened by a byte.

`tests/harness.rs` holds one test that reads both — `the_two_corpora_share_no_vocabulary`
— and its job is to keep them apart.

## The vocabulary

Every event is `{"kind": …, "body": {…}}`. It shares **no** word with the other
corpus, and that is the point rather than decoration: shared code that reads
`type`, `terminal_reason`, `subtype` or `rate_limit_info` produces an opaque
`Other` event here and fails a test, instead of quietly appearing to work.

| Event | Means |
| --- | --- |
| `conversation.opened` | the provider announces the id **it** minted |
| `agent.said` / `agent.tool` | one turn's text, or one tool call |
| `tool.returned` | that call's result |
| `window` | the usage window, **relative** (`reopens_in_s`) and reported every turn |
| `finished` | the ending: `why` is `ok`, `stopped`, `budget` or `window_closed` |

`finished` carries **no cost in dollars** — tokens only. That is deliberate
pressure on seam-contract D18: a provider that reports no dollar figure must reach
the `runs` row as NULL and mean "not recorded", never zero.

## The eight scenarios

| File | What it is |
| --- | --- |
| `finished.jsonl` | a clean run that opens a pull request |
| `stopped.jsonl` | killed mid-stream; the ending still arrives before the exit |
| `budget.jsonl` | the step budget ran out — fatal, never retried |
| `window-closed.jsonl` | the wall, with a relative reopen |
| `window-closed-no-reopen.jsonl` | the wall, with no reopen at all — the fixed-poll fallback |
| `continued.jsonl` | a second attempt of the same conversation |
| `torn.jsonl` | a writer killed mid-line: no ending, and a final line that is not JSON |
| `unknown-kind.jsonl` | two events this version does not model, tolerated and kept whole |

Adding a scenario changes this directory and nothing else — the loader globs
`*.jsonl` per corpus.
