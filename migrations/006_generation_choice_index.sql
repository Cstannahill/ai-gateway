-- Separates per-choice generation content instead of merging every choice
-- of an `n > 1` attempt into one row with no index at all. Existing rows
-- default to choice_index 0 (the only choice that existed before `n > 1`
-- support), which is the correct backfill value since single-choice was
-- the only case ever written prior to this migration.
--
-- `status` distinguishes a generation reached via a genuine terminal
-- `Completed` event ('complete') from one reached via a mid-stream `Error`
-- with partial content preserved ('partial') — previously there was no way
-- to tell these apart after the fact.
--
-- See docs/frontend/API_GAPS.md item 5's choice-ID gap.

ALTER TABLE generations ADD COLUMN choice_index INTEGER NOT NULL DEFAULT 0;
ALTER TABLE generations ADD COLUMN status TEXT NOT NULL DEFAULT 'complete';
