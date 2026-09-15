# 24. A calm interface, and one that travels to the web

- **Status:** Accepted
- **Date:** 2026-09-15

## Context

`src/styles.css` opens with a block headed THE DIRECTION, and it has been the design contract
since the shell was built. Its second paragraph reads:

> Dense, instrument-like. Closer to a terminal or Linear than a form: more information per
> screen, tighter vertical rhythm.

The app delivers on it. The board puts nine elements on every card, repository names are set in
monospace, run states are filled uppercase pills, cards carry a coloured rail *and* a tinted
background, and both row actions sit permanently under every card. Across the four leaf
stylesheets there are **41 `text-transform: uppercase` declarations**.

Three things have changed since that was written.

**The instrument reading is now the problem, not the goal.** "Dense and instrument-like" and
"reads as a tool built by developers for developers" are the same surface described
approvingly and disapprovingly. Uppercase-with-wide-tracking, monospace identifiers and
saturated state pills are the specific grammar of that reading, and they are applied to
material that is not code: a repository is a name, not a command; "waiting for retry" is a
sentence, not a log level.

**The interface is going to the web.** The same design has to work in a browser, at widths
the desktop window never sees, where a first-time visitor forms an opinion in seconds and
where light mode is not a courtesy. A layout tuned for one window size on one platform does
not survive that move, and neither does a palette only ever judged on dark.

**Density was never the scarce resource.** The board holds a few dozen cards, and the two
moments the product is designed around — queue in the evening, review in the morning — are
both *reading* moments. Fitting four more cards on screen was bought with contrast, hierarchy
and calm, and the trade was the wrong way round.

## Decision

**Replace "dense, instrument-like" with: calm, legible, and unremarkable in the way good
consumer software is unremarkable.** Six rules, each aimed at a signal that is actually in the
stylesheets today.

### 1. Sentence case. No uppercase transforms

`text-transform: uppercase` paired with `--letter-spacing-wide` is the loudest single signal in
the app and the cheapest to remove. Column headings, badges, section labels and eyebrows all
become sentence case at their natural width. `--letter-spacing-wide` stays in the token set for
the rare true eyebrow, but the default is that a label is just words.

### 2. Monospace means "literal text you could copy"

A transcript, a shell command, a branch name, a token count in a column that must align. It
does **not** mean "this is an identifier" — a repository name set in monospace is a name
wearing a costume. `.task-card-repo` loses it; `doctor.css`'s command block and `runs.css`'s
transcript keep it, because there the glyphs are the point.

### 3. State is a dot and a word, not a filled pill

ADR-0007's `run_state` still drives everything; only its presentation changes. A dot in the
status colour plus the state in sentence case, and the colour is never the only carrier — the
word is always there. The card's coloured rail and its background wash both go: two redundant
encodings of the same fact, and the wash is what makes a column of failures look like an
incident rather than a morning's review.

### 4. Colour is used sparingly enough that it still means something

One accent. Status colours desaturated to sit *in* the surface rather than on top of it. The
current palette puts saturated blue, red, purple, amber and green on one screen at once, which
spends the whole budget before anything important happens.

### 5. Actions appear when they are relevant

Permanent per-card buttons are a toolbar pretending to be content. Row actions reveal on hover
and on `:focus-within`, and remain reachable by keyboard — the reveal is presentation, never
the only route to the action.

### 6. The layout is fluid, and light mode is equal

No fixed window assumption: the shell collapses its sidebar below a breakpoint, the board
scrolls horizontally with a column minimum rather than a fixed count, and content has a
max-width so a wide browser does not stretch a line of text to 200 characters. Light mode is
designed, not derived — same structure, contrast checked to WCAG AA on both.

## Consequences

- THE DIRECTION block in `src/styles.css` is rewritten to state the above. It remains the place
  a leaf stylesheet is told what to do, so it cannot be left describing the old contract.
- Token *values* change — surfaces, accent, status colours, radii, shadows, type sizes. Token
  *names* do not, so the four leaf stylesheets keep resolving, and the diff stays reviewable
  instead of being five thousand lines of renaming.
- The legacy aliases (`--bg`, `--surface`, `--border`, `--text`, `--text-muted`) stay pointed at
  the new scale, for the same reason they were kept before.
- **No behaviour changes.** No component gains or loses a capability, no command or MCP tool
  moves, and the vitest suite is expected to pass untouched except where it asserts on text that
  was uppercase only by CSS. A test that breaks on this change is a test that was asserting a
  presentation detail.
- ADR-0002's desktop-first framing is unaffected: this prepares the *design* for a browser, not
  the packaging. Nothing here ships a web build or moves a line of Rust.

## Alternatives considered

**Keep the direction and restyle inside it.** This is what the two previous UI chains did, and
task 028's post-mortem is blunt about the result: one produced 252 token substitutions and two
lines of visible change, the other moved the design but could not see what it had done. The
brief was the variable both times. Restyling inside a direction that asks for a terminal
produces a terminal.

**Adopt a design system (shadcn, Radix themes, Tailwind).** Rejected on the same grounds
seam-contract D6 rejects casual dependencies: the app has no UI library by choice, the token
layer already exists and is well documented, and the work here is judgement about a dozen
values rather than a component library. A framework would also make the web move harder, not
easier, by adding a second opinion about layout to the one being written down here.

**Copy an interface that already feels right.** Tempting, and the reason this entry names
qualities — calm, sentence case, sparing colour, fluid layout — rather than a reference. A
copied surface arrives without the arguments that produced it, so the next decision has nothing
to reason from.
