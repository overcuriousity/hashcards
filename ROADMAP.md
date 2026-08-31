# UX Roadmap

Companion document: `SPEC.md` (bugs and features for the big update; IDs
referenced below). This roadmap covers user-experience improvements,
ordered by phase. Each item is small enough to land independently.

## Phase 1 — Honest feedback (highest impact)

Depends on FEAT-01 (flash messages) from the spec.

- **Surface every failure and every silent no-op.** Today a failed grade,
  bookmark, sync, HedgeDoc add, or note edit looks identical to success.
  Once flash messages exist, route all of them through it (BUG-03).
- **"Nothing due" drill starts say so.** Starting a drill with zero due
  cards currently redirects back to the same page unchanged
  (`handlers.rs:289-291`). Show "Nothing due in the selected decks."
- **Style the drill error page and give it an exit.** It is currently an
  unstyled dead end returning HTTP 200 (`get.rs:56-63`, BUG-51).

## Phase 2 — Navigation and reachability

- **Collection names on the landing page are always links.** Today a
  collection with nothing due has no link at all (`landing.rs:92-98`),
  making browse, bookmarks, and edit unreachable. Link the name to
  `/collection/{slug}`; keep the Drill button conditional on due > 0.
- **Card browser with edit links.** Editing is currently reachable only
  from the bookmark list. Add a per-deck card list (question preview +
  edit link) on the browse page or behind a "Cards" tab. This also serves
  the "preview command" idea from IDEAS.md.
- **Search.** A simple substring filter over card text in the card
  browser. Client-side is fine at typical collection sizes.
- **Edit page remembers where you came from.** Back-links are hardwired
  to the bookmark list (`edit.rs:85`, `:112`, `:329`); pass and honor a
  `return` parameter.
- **Bookmarks reachable in drill mode.** In `hashcards drill` bookmarks
  are write-only (no route exists, drill `server.rs:192-207`). Add the
  bookmark list + edit routes to the drill server, or state clearly in
  the UI that bookmarks are reviewed via `serve`.

## Phase 3 — Drill session quality

- **Progress text with repeat-aware bar** (FEAT-08): "7 of 20 (+3
  repeats)". Today the bar silently stalls whenever a card is re-queued
  by Forgot/Hard (`get.rs:90`) and there is no numeric indicator at all.
- **Differentiate the grade buttons.** All four grades are identical
  white buttons (`style.css:239-248`). Give Forgot a distinct (red-ish)
  treatment and Easy a subtle one; keep the flat aesthetic.
- **Confirm End; separate destructive controls.** `End` and `Shutdown`
  are one-click and sit in the same stacked column as the grades on
  mobile (`get.rs:301`, `:392`). Add a lightweight confirm (or a
  press-and-hold) and visually separate them from grading.
- **Fix keyboard behavior.** Space currently hijacks native button
  activation (a keyboard user focused on Bookmark gets Reveal instead,
  `script.js:61-71`); shortcuts fire even while a button is focused; held
  keys auto-repeat (BUG-06). Respect focused controls, check
  `event.repeat`.
- **Discoverable shortcuts.** Shortcuts exist only as `title` tooltips
  (`get.rs:113-116`), invisible on touch. Add a small "?" popover or a
  footer line listing space/1-4/u/b, and add a shortcut for End.
- **Stale-page protection.** Browser Back shows an old card; grading it
  applies to the current one. The card-hash token from BUG-06 lets the
  server detect and flash "page was out of date" instead.
- **Limit dropdown updates the Drill label.** Selecting "10" should make
  the button read "Drill (10)" instead of "Drill (247 due)"
  (`browse.rs:316`).

## Phase 4 — Editing experience

- **Never lose typed text.** On stale-mtime, parse, or active-session
  errors, re-render the edit form with the submitted content and the
  error inline (`edit.rs:318-336` currently discards it).
- **Consistent active-session policy.** GET warns but POST hard-fails
  (`edit.rs:91-95` vs `:148-150`). Pick one: ideally allow the edit and
  refresh the in-memory session, otherwise block at GET with the same
  message.
- **Show migration outcomes.** After an edit that renames hashes or
  orphans history (BUG-35), tell the user what happened to their review
  history instead of migrating silently.

## Phase 5 — Ambient quality

- **Live landing counts** (BUG-45): refresh due counts when a session
  ends and on landing GET after staleness.
- **Session resume prompt** (FEAT-03): "Resume session (N cards
  remaining)?" instead of silently discarding an interrupted drill.
- **Stats page** (FEAT-02): due forecast, review history, grade
  distribution — the biggest single feature gap versus other SRS tools.
- **PWA Phase B** (from PLAN.md): service worker caching for static
  assets, offline notice page. Phase A (manifest, icons) already shipped.
- **Logo/favicon** (from IDEAS.md): replaces the placeholder PWA icons.

## Explicitly deferred

- Swipe-to-grade gestures on mobile: revisit after Phase 3 lands and the
  button layout settles.
- Fully offline drilling: state is server-side by design; would need a
  client-side FSRS + sync protocol. Out of scope for this cycle.
- Multi-user auth: single-user assumption stands (BUG-47 limits exposure
  to localhost by default); revisit only if deployment stories demand it.
