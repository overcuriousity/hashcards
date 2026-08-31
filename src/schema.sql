pragma foreign_keys = on;

create table cards (
    card_hash text primary key,
    added_at text not null,
    last_reviewed_at text,
    stability real,
    difficulty real,
    interval_raw real,
    interval_days integer,
    due_date text,
    review_count integer not null
) strict;

create table sessions (
    session_id integer primary key,
    started_at text not null,
    ended_at text not null
) strict;

create table reviews (
    review_id integer primary key,
    session_id integer not null
        references sessions (session_id)
        on update cascade
        on delete cascade,
    card_hash text not null
        references cards (card_hash)
        on update cascade
        on delete cascade,
    reviewed_at text not null,
    grade text not null,
    stability real not null,
    difficulty real not null,
    interval_raw real not null,
    interval_days integer not null,
    due_date text not null,
    duration_ms integer,
    voided integer not null default 0
) strict;

create table bookmarks (
    card_hash text primary key
        references cards (card_hash)
        on update cascade
        on delete cascade,
    note text,
    created_at text not null
) strict;

create index idx_reviews_card_hash on reviews (card_hash);
create index idx_reviews_session_id on reviews (session_id);

create table schema_version (
    version integer not null
) strict;
