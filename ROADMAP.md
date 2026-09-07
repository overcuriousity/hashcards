# Roadmap

Ordered. Items are worked top to bottom; anything unordered lives in
`IDEAS.md`.

## 0. Ending a session must always be possible

**Blocking everything below.** Reported from a phone: mid-session, on a train,
with no way to stop. A screenshot of the pre-reveal drill screen (v0.4.10 UI:
back-arrow and star in the header, `Reveal` pinned to the bottom bar) shows no
`End session` line under the button. The same view in a desktop browser has it.

What the code says: `end_button()` sits in `div.controls` outside the
reveal conditional (`src/cmd/drill/get.rs:162`), and has in every commit in
the history — `render_session_page` is the only renderer of the drill screen.
So the markup should carry `id="end"` on both the phone and the desktop.
Not yet explained. Needs the page source as the phone actually received it.

Two defects found while looking, both real regardless of the report, both
now fixed:

- ~~`.end-link` never got its own styling.~~ `.controls button`
  (specificity 0-1-1) outranked `.end-link` (0-1-0) on every property the two
  share, so the way out of a session rendered as a bordered 44px button
  rather than the quiet line the rule was written to produce. The grade rules
  now exclude it by name.
- ~~`/style.css` was served `immutable` from an unversioned path.~~ It is now
  content addressed, so `immutable` is honest and a deploy invalidates it.
  Note this was only ever true of the stylesheet: `/script.js` and a
  collection's `script.js` sent no cache header at all, which left them to
  heuristic caching; all three now revalidate or carry a hash.

Neither explains the screenshot. The stylesheet in it is demonstrably the
current one — the header is a three-column grid with the arrow and star at
the far corners, which the previous `display: flex; justify-content: center`
could not produce — so a stale cached stylesheet is ruled out, and the End
button contributes zero height rather than being clipped: measuring the
screenshot puts the bottom of the controls bar at the top of the gesture bar,
with the 14 physical px of padding the layout predicts and no room taken by
anything else.

Also worth doing here: **there is no service worker.** The manifest
(`template.rs:28`) makes the app installable, but every reveal, grade and
`End` is a form POST that needs the network. On a train, the whole session
locks up — which is the situation the report came from, whatever the missing
button turns out to be.

## 1. FSRS

`src/fsrs.rs` is a real FSRS-5: the nineteen stock weights, the power-law
forgetting curve (`F = 19/81`, `C = -0.5`), stability-on-success and
stability-on-failure kept apart, difficulty with mean reversion, and ±5%
interval fuzz (`types/performance.rs:47`). What is missing is everything
around it.

1. **Desired retention is a constant.** `TARGET_RECALL = 0.9`
   (`types/performance.rs:36`). It belongs in a collection's configuration:
   it is the one FSRS knob a user has an opinion about, and the trade it
   makes — more reviews for better recall — is the whole point of the
   algorithm being tunable.
2. **No maximum interval.** Nothing caps the interval, so a card graded
   `Easy` a few times can leave for a decade. A per-collection ceiling,
   applied after the fuzz.
3. **No parameter optimisation.** The weights are the published defaults, so
   the scheduler is calibrated to a population rather than to the person
   using it. The `reviews` table already stores every input an optimiser
   needs — grade, stability, difficulty, timestamps, and a `voided` flag to
   exclude undone rows — so this is a computation over data already held,
   not a schema change. Report the fitted weights and let them be adopted or
   discarded, rather than swapping them in silently.
4. **No learning or relearning steps.** A forgotten card is requeued inside
   the session, but there is no notion of same-day steps, and no separate
   path for a card that lapsed after being mature. This overlaps item 5
   below and is best done with it.

## 2. Scheduling and the card lifecycle

The largest gap against Anki, and the one users notice without knowing the
vocabulary. The `cards` table (`src/schema.sql`) has `review_count` and
nothing else: a card seen once and a card held for two years are the same
kind of row.

1. **Card states.** New, learning, review, relearning, as an explicit state
   machine rather than a count. Everything else in this section needs it,
   and so does FSRS item 4. `IDEAS.md` has carried this since the fork.
2. **Suspend and bury.** There is no way to take a card out of rotation.
   Bookmark is a note to self, not a scheduling act — a card you cannot
   answer today should be droppable for the day (bury) or indefinitely
   (suspend), and both are reversible from the browse page.
3. **Leeches.** No lapse counter exists, so a card that is failed forever is
   failed forever in silence. Count lapses, and at a threshold suspend the
   card and say so: the answer to a card you cannot learn is to rewrite it,
   which now takes one tap.
4. **Per-day limits.** The session-size dropdown
   (`cmd/serve/browse.rs:277`) caps one sitting. Anki's limits are per day
   and persist across sittings, which is what actually keeps a backlog from
   becoming unfaceable — separate caps for new cards and reviews.
5. **Manual overrides.** Forget (reset a card to new), set due date, and
   reschedule. Each is a small write against `cards`, and each is the escape
   hatch for a schedule that has gone wrong — which, without item 3 above,
   it eventually will.
6. **Filtered study.** Cram a topic before an exam, or re-drill today's
   failures, without disturbing the real schedule. Saved decks are a static
   selection; this is a query, and it wants the card states from item 1.
