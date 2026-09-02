# hashcards-web

[![Test](https://github.com/overcuriousity/hashcards-web/actions/workflows/test.yaml/badge.svg)](https://github.com/overcuriousity/hashcards-web/actions/workflows/test.yaml)
[![Release](https://github.com/overcuriousity/hashcards-web/actions/workflows/release.yaml/badge.svg?branch=master)](https://github.com/overcuriousity/hashcards-web/actions/workflows/release.yaml)
[![dependency status](https://deps.rs/repo/github/overcuriousity/hashcards-web/status.svg)](https://deps.rs/repo/github/overcuriousity/hashcards-web)

![Screenshot of the app, showing a front/back flashcard.](screenshot.webp)

A multi-user web server for plain text spaced repetition. Point it at a git
repository of Markdown files and it serves them as flashcard collections,
scheduling reviews with [FSRS] and keeping every user's history in SQLite.

- **Plain text, in your repository.** Cards are Markdown files you write in
  your own editor and track in git. The server clones the repository and
  syncs it on a timer; edits made in the browser are committed back.
- **Content addressed.** A card is identified by the hash of its text, so
  editing a card is a deliberate act with visible consequences for its
  schedule — nothing is silently rewritten behind your back.
- **Multi-user.** With an `[oidc]` section, every route is gated behind
  login and each collection belongs to exactly one owner.
- **Nothing to install for readers.** Reviewing happens in any browser.
  There is no client, no sync protocol, and no account to create.

This is a fork of [hashcards] by [Fernando Borretti][fb], which is a local
command-line tool. The card format, the parser, the FSRS implementation and
the review schema are all his work; see [Prior Art](#prior-art). This fork
removed the command-line interface and grew the server: HedgeDoc note
sources, cross-collection decks, in-browser editing, and OIDC login.

## Quick start

```bash
$ curl -fsSL https://raw.githubusercontent.com/overcuriousity/hashcards-web/master/install.sh | sh
$ cp hashcards.example.toml hashcards.toml   # then edit it
$ hashcards-web --config hashcards.toml
```

The server reads everything from the configuration file — the bind address,
the collections, the git remote, the login settings. There are no other
command-line options:

```
hashcards-web [--config <path>]
```

With no `--config`, `hashcards.toml` in the current directory is used. If
there is no configuration file, the server refuses to start rather than
guessing.

## Installation

### From a release

```bash
$ curl -fsSL https://raw.githubusercontent.com/overcuriousity/hashcards-web/master/install.sh | sh
```

Installs the latest release binary to `~/.local/bin` (override with
`INSTALL_DIR`). Linux amd64, macOS arm64 and Windows amd64 are published.

### From source

Requires a [Rust toolchain][rustup] and `make` (which downloads and trims the
vendored KaTeX distribution):

```bash
$ make
$ sudo make install          # installs to /usr/local/bin
```

`make example` serves the bundled example collection at
<http://127.0.0.1:8000> so you can see the thing running before writing any
configuration.

## Configuration

`hashcards.example.toml` is the annotated reference; this section explains
what each part is for. Everything except `[server].data_dir` and at least one
collection has a working default.

### `[server]`

```toml
[server]
host = "127.0.0.1"                  # default; see the warning below
port = 8000
data_dir = "/var/lib/hashcards"     # required
session_timeout_minutes = 1440      # 0 disables eviction
```

`data_dir` is where the server keeps the repository clone (`{data_dir}/repo`),
the review databases (`{data_dir}/db`) and any HedgeDoc notes
(`{data_dir}/hedgedoc`). Collection paths are always resolved inside
`{data_dir}/repo`.

The server creates all of these at startup, and refuses to start with a message
naming the directory if it cannot. **It must be writable by the user the server
runs as.** `/var/lib/hashcards` is the conventional choice for a system
service, but nothing creates it for you unless the systemd unit says so:

```ini
[Service]
User=hashcards
StateDirectory=hashcards          # creates /var/lib/hashcards owned by User=
WorkingDirectory=/var/lib/hashcards
ExecStart=/usr/local/bin/hashcards-web --config /etc/hashcards/hashcards.toml
```

Without `StateDirectory=` (or an equivalent `mkdir` + `chown`), `/var/lib` is
root-owned and the server cannot write there. Running as your own user? Point
`data_dir` somewhere you already own, such as `~/.local/share/hashcards`.

`session_timeout_minutes` evicts drill sessions left idle that long and closes
their database session row. Nothing is lost: every grade is written the moment
it happens, so an evicted session keeps all its progress.

**On binding to the network.** The default `127.0.0.1` is reachable only from
the machine itself. Setting `host = "0.0.0.0"` exposes the server, and without
an `[oidc]` section there is no authentication whatsoever — anyone who can
reach the port can read your cards and edit the underlying files. Expose it
only behind an authenticating reverse proxy, or configure OIDC.

### `[git]`

```toml
[git]
repo_url = "https://github.com/user/flashcards.git"
branch = "main"
poll_interval_minutes = 30          # 0 disables polling
commit_author_name = "hashcards web edit"
commit_author_email = "hashcards@localhost"
```

The repository is cloned into `{data_dir}/repo` at startup and pulled on the
interval; the landing page also has a "Sync Now" button. Cards edited in the
browser are committed with the configured author — or, when OIDC is on, as the
logged-in user.

The section is optional. Without it there is no syncing, and you are
responsible for putting the cards under `{data_dir}/repo` yourself.

### `[[collection]]`

```toml
[[collection]]
name = "Japanese"
path = "japanese"
# owner = "me@example.com"          # required when [oidc] is configured
```

Each collection is a directory of Markdown files under `{data_dir}/repo`, with
its own review database at `{data_dir}/db/{slug}.db`. The slug is derived from
`path`, so `medicine/anatomy` becomes `medicine-anatomy`; two collections whose
paths produce the same slug are rejected at startup rather than silently
sharing a database.

### `[[hedgedoc]]`

```toml
[[hedgedoc]]
url = "https://notes.example.com/q1QcHIRvQFiTxzwnF-_L3g"
# owner = "me@example.com"
```

A [HedgeDoc] note served as a collection of its own. The server fetches the raw
Markdown from `{url}/download`, takes the collection name from the document
title, and re-fetches on the same interval as git. Notes can also be added and
removed from the web interface, which writes them back to this file.

Each note is its own collection, owned by that entry's owner alone. Notes are
never grouped by HedgeDoc host, so several people can take notes from one
shared instance — including the same note — without sharing a collection or a
review database.

### `[[deck]]`

```toml
[[deck]]
name = "Exam revision"
members = ["japanese/Verbs", "medicine-anatomy/Bones"]
# owner = "me@example.com"
```

A *deck* is a saved selection of decks drawn from any of your collections,
drilled together in one session. Manage them at `/decks`.

A deck owns no cards and no database. Drilling one opens each contributing
collection's own database and routes every review back to the collection the
card came from, so **a card keeps exactly one schedule** however many decks
include it — it never becomes due twice on schedules that drift apart.
Deleting a deck removes only the selection.

### `[defaults]`

```toml
[defaults]
answer_controls = "full"            # "full" or "binary"
bury_siblings = true
jitter = 0.05
```

- `answer_controls`: `"full"` shows four grading buttons (Forgot / Hard /
  Good / Easy); `"binary"` shows only Forgot and Good.
- `bury_siblings`: show at most one card per cloze group per session, so one
  deletion's text does not spoil its sibling's answer.
- `jitter`: random ±fraction applied to review intervals to spread review
  peaks. `0.0` to `0.5`.

### `[oidc]`

```toml
[oidc]
issuer_url = "https://cloud.example.com/index.php/apps/oidc"
client_id = "..."
client_secret = "..."
external_url = "https://hashcards.example.com"
session_secret = "..."              # at least 32 bytes
# scopes = ["openid", "email", "profile"]
```

Adding this section turns on login for every route except `/auth/*`, and
requires every `[[collection]]`, `[[hedgedoc]]` and `[[deck]]` entry to declare
an `owner` — an email, matched case-insensitively against the OIDC `email`
claim. Config load fails if any entry is missing one, and equally if an `owner`
appears *without* an `[oidc]` section, since nobody would ever be logged in to
match it.

- `external_url` is the address a browser actually reaches the server at, even
  behind a reverse proxy. It is independent of `host`/`port`, and the redirect
  URI you register with your provider is `{external_url}/auth/callback`.
- `session_secret` signs the session cookie. It must be at least 32 bytes
  (`openssl rand -hex 32`); config load fails otherwise. Rotating it logs out
  every user.
- The session cookie is `HttpOnly` and `SameSite=Lax`, lasts 30 days (re-issued
  while you keep using it), and is marked `Secure` when `external_url` is
  HTTPS.
- Adding a user is a config edit plus a restart. There is no signup flow, no
  admin UI, and no sharing: each collection is visible to exactly one owner. A
  logged-in user who owns nothing sees an empty landing page.
- Log out from the button on the landing page. `/auth/logout` is a POST, so a
  third-party page cannot trigger it.

Without `[oidc]`, the server assumes a single user. Drill sessions are keyed by
collection, so two browsers pointed at the same collection share one session:
both see the same card, and a grade from either advances the shared queue.

## Using it

The landing page lists your collections with the number of cards due. Opening
one shows its deck tree; select decks and start a drill. From there:

| Route | What it does |
|---|---|
| `/` | Collections, due counts, sync, logout |
| `/collection/{slug}` | Deck tree, duplicate warnings, start a drill |
| `/collection/{slug}/stats` | Due forecast, review history, grade distribution |
| `/collection/{slug}/export` | The whole collection as JSON |
| `/collection/{slug}/bookmarks` | Cards you flagged while drilling |
| `/decks` | Create and delete cross-collection decks |
| `/hedgedoc` | Add and remove HedgeDoc note sources |

**Editing.** Bookmark a card during a drill (shortcut: `b`), then edit it from
the bookmark list. Edits are written to the Markdown file and committed to git.
Because cards are content addressed, editing changes a card's hash; the server
migrates the review history to the new hash where it can and tells you when it
cannot.

**Export.** `/collection/{slug}/export` returns every card, its scheduling
state, and the full review history as JSON. Your Markdown lives in git, but the
review databases live under `data_dir` and are in nobody's repository — and
under OIDC you have no filesystem access — so this is how you get your own
history out.

**Duplicates.** Byte-identical cards are deduplicated when a collection loads:
one copy is dropped, and only the other carries review history. The collection
page names any it finds, with both file locations.

## Card format

### Basic cards

```
Q: What are the possible values of electric charge?
A: Any integer multiple of the fundamental charge.
```

Both sides can span multiple lines:

```
Q: List the elements of the Platinum group.
A:

- ruthenium
- rhodium
- palladium
- osmium
- iridium
- platinum
```

### Cloze cards

Cloze cards start with `C:` and use square brackets for deletions:

```
C: The [order] of a group is [the cardinality of its underlying set].
```

They can span multiple lines too:

```
C:
Better is the sight of the eyes than the wandering of the
desire: this is also vanity and vexation of spirit.

— [Ecclesiastes] [6]:[9]
```

Square brackets are reserved for deletions inside `C:` cards. The exact rules:

- `[text]` marks a deletion. It must be non-empty and must close on the same
  line it opens.
- `\[` and `\]` produce literal square brackets.
- Image syntax (`![alt](path)`) is passed through to Markdown untouched.
- Link syntax (`[text](url)`) is passed through untouched: a bracket group
  immediately followed by `(` is a link, not a deletion.
- Nested brackets (`[[a]]`) and deletions left open at the end of a line are
  parse errors.

Each bracketed deletion becomes its own card. A cloze card's hash is derived
from the card's text, the deleted substring, and — when the same substring is
deleted more than once — an occurrence index. It does not depend on byte
offsets or on the machine's CPU architecture, so a database written on one
computer works on any other.

### Term-definition cards

```
T: Monoid
D: A semigroup with an identity element.
```

Shorthand: at parse time this expands into two ordinary cards, one in each
direction.

```
Q: Define: Monoid
A: A semigroup with an identity element.

---

Q: Term for: A semigroup with an identity element.
A: Monoid
```

The generated cards are indistinguishable from hand-written ones — same
content, same hashes — so converting between the shorthand and the explicit
form preserves review history.

Lines starting with `T:` or `D:` are card tags everywhere, exactly like `Q:`
and `A:`. To use such text literally inside a card, don't start a line with it.

### Separators

Cards may optionally be separated by horizontal rules:

```
C: A semigroup with an identity element is called a [monoid].

---

C: A semigroup without associativity is called a [magma].
```

### LaTeX

Math is rendered with KaTeX. Use `$...$` inline and `$$...$$` for display:

```
C: The [amount of substance] of a sample, denoted $n$, is defined as:

$$
n = \frac{N}{N_A}
$$

where $N$ is [the number of elementary entities] and $N_A$ is [Avogadro's constant].
```

Custom macros go in a `macros.tex` file at the collection root, one per line.
Definitions may take arguments (`#1`, `#2`, …):

```
\C \mathbb{C}
\R \mathbb{R}
```

### Images and audio

Ordinary Markdown image syntax works for both:

```
Q: Identify this painting:

![](art/diagram.png)

A: _The Siren_, by John William Waterhouse.
```

```
Q: How do you pronounce "پرنده" in Persian?
A: ![](audio/parande.mp3)
```

Paths resolve relative to the Markdown file containing the card. Prefixing a
path with `@/` resolves it relative to the collection root instead, so the
reference survives the file being moved:

```
cards/
  Art Theory/
    Art.md            # can use Images/Circe.jpg
    Images/
      Circe.jpg       # or @/Art Theory/Images/Circe.jpg from anywhere
```

Media files are validated when a collection loads, and served through
`/file/{path}` with path traversal blocked.

### Deck names

A deck is named after its filename: `Medicine.md` is the deck `Medicine`.
Override that with TOML frontmatter:

```
---
name = "Medicine"
---

C: The mitochondria is the [powerhouse] of the cell.
```

This lets many files share one deck name — useful when taking notes from a
book chapter by chapter:

```
Principles of Neural Science/
  Ch1.md
  Ch2.md
```

## Database

Each collection has an SQLite database at `{data_dir}/db/{slug}.db`. Reviews
are written as they happen and in the same transaction as the card's
performance, so an interrupted session keeps its progress. Undo marks a review
`voided` rather than deleting it, and read paths filter on `voided = 0`.

The `cards` table:

| Column             | Type               | Description                                                                                                                        |
|--------------------|--------------------|------------------------------------------------------------------------------------------------------------------------------------|
| `card_hash`        | `text primary key` | The hash of the card.                                                                                                              |
| `added_at`         | `text not null`    | When the card was first added to the database.                                                                                     |
| `last_reviewed_at` | `text`             | When the card was most recently reviewed. `null` if the card is new.                                                               |
| `stability`        | `real`             | The card's stability. `null` if the card is new.                                                                                   |
| `difficulty`       | `real`             | The card's difficulty. `null` if the card is new.                                                                                  |
| `interval_raw`     | `real`             | The FSRS-calculated interval, before rounding and clamping, in days. `null` if the card is new.                                    |
| `interval_days`    | `real`             | The interval as an integer number of days, after rounding and clamping. `null` if the card is new.                                 |
| `due_date`         | `text`             | When the card is next due, `YYYY-MM-DD`. `null` if the card is new.                                                                |
| `review_count`     | `integer not null` | How many times the card has been reviewed.                                                                                         |

The `sessions` table:

| Column       | Type                  | Description                        |
|--------------|-----------------------|------------------------------------|
| `session_id` | `integer primary key` | The ID of the session.             |
| `started_at` | `text not null`       | When the session started.          |
| `ended_at`   | `text not null`       | When the session ended.            |

The `reviews` table:

| Column          | Type                  | Description                                                                                        |
|-----------------|-----------------------|----------------------------------------------------------------------------------------------------|
| `review_id`     | `integer primary key` | The review ID.                                                                                     |
| `session_id`    | `integer not null`    | The session this review was performed in, a foreign key.                                           |
| `card_hash`     | `text not null`       | The card that was reviewed, a foreign key.                                                         |
| `reviewed_at`   | `text not null`       | When the grade was submitted.                                                                      |
| `grade`         | `text not null`       | One of `forgot`, `hard`, `good`, or `easy`.                                                        |
| `stability`     | `real not null`       | The card's stability after this review.                                                            |
| `difficulty`    | `real not null`       | The card's difficulty after this review.                                                           |
| `interval_raw`  | `real`                | The FSRS-calculated interval, before rounding and clamping, in days.                               |
| `interval_days` | `real`                | The interval as an integer number of days, after rounding and clamping.                            |
| `due_date`      | `text not null`       | When the card is next due, `YYYY-MM-DD`.                                                           |

Timestamps are `YYYY-MM-DDTHH:MM:SS.MMM`, e.g. `2025-10-04T17:09:51.517`.
Dates are naive by design: a due date is closer to the date on a journal entry
than to a precise point in time, so there are no timezones anywhere.

## Prior art

hashcards-web is a fork of [hashcards] by [Fernando Borretti][fb]
([announcement post][blog]). His [essay on effective spaced repetition][esr]
explains the reasoning behind the design.

- [org-fc](https://github.com/l3kn/org-fc)
- [org-drill](https://orgmode.org/worg/org-contrib/org-drill.html)
- [hascard](https://hackage.haskell.org/package/hascard)
- [carddown](https://github.com/martintrojer/carddown)
- [My implementation of a personal mnemonic medium](https://notes.andymatuschak.org/My_implementation_of_a_personal_mnemonic_medium)

[FSRS]: https://github.com/open-spaced-repetition/fsrs4anki
[HedgeDoc]: https://hedgedoc.org/
[hashcards]: https://github.com/eudoxia0/hashcards
[blog]: https://borretti.me/article/hashcards-plain-text-spaced-repetition
[esr]: https://borretti.me/article/effective-spaced-repetition
[rustup]: https://rustup.rs/

## License

© 2025 by [Fernando Borretti][fb], and contributors to this fork. Licensed
under the [Apache 2.0][apache2] license.

[fb]: https://borretti.me/
[apache2]: https://www.apache.org/licenses/LICENSE-2.0
