# Plan: Spotlight Wave A — ranking signals

Status: draft, pending Momus review.
Scope: one wave. Covers ranking only. Calculator-as-plugin, shell plugin,
OCR, and semantic search are out of scope (separate plans).

## Goal

Make result ranking feel Spotlight-like: what a user wants at row 1 is at
row 1, and the system learns from what the user actually opens. Achieve
this with transparent, configurable multiplicative signals on top of the
existing Tantivy BM25 score, without ML and without breaking any of the
existing SC-1..SC-22 parity criteria.

## Non-goals

- Moving calculator out of the daemon. Calculator stays exactly as it is.
- New source plugins. The plugin trait is not extended in this wave.
- Learning-to-rank. No ML, no online learning, no bandit. Static weights
  configurable in `[ranking]`.
- Semantic / vector search. Wave A is lexical + frecency + latching only.
- Replacing the GTK4 result list widget. Wave A adds a hero row above it;
  it does not touch how rows inside the list are rendered.

## User-facing outcome

- `sqrt(16)+pi` still works (calculator unchanged).
- Typing the first 2–3 chars of an app or file I use often surfaces it
  as Top Hit in a dedicated hero row above the results list.
- Typing an acronym of an app name (`vsc` → Visual Studio Code, `gic` →
  Google Image Capture) matches.
- Recently-touched files outrank old ones for the same lexical score.
- The more I click a specific doc for a specific query, the more that
  pair sticks ("query latching" — Alfred/Spotlight-style).
- Ambiguous queries do *not* get a Top Hit row at all; the UI shows
  just a ranked list, which is how Spotlight behaves when confidence is
  low.

## Guiding principles

1. **Multiplicative composition, no additives.** The current
   `score += history.bonus()` path is an anomaly; it is replaced.
2. **Split by data source, not by crate convenience.** Signals derivable
   from the Tantivy document live in `lixun-index::search`, next to the
   existing `score * category.ranking_boost()`. Signals derived from
   external state (click history, query latches) live in the daemon
   post-processing step.
3. **Configurable everything, defaults that pass SC-1..SC-22.** Every
   weight, every threshold, every cap is a key in `[ranking]` with a
   documented default.
4. **No host branches on concrete plugins or per-category domain logic.**
   Per-category multipliers live in config (not hardcoded). Per-category
   gating of a signal (e.g. "prefix boost off for mail") is explicitly
   out of scope for Wave A; see decision D2.
5. **Fail-safe rollback.** Every signal has a weight that, when set to
   its neutral value (1.0 for mults, 0.0 for alphas), reproduces pre-Wave
   A behavior exactly. This is tested.
6. **No AI-agent names anywhere.** Commits, code, comments — bound by
   `AGENTS.md`. Commit author stays `Denis Kopin <denis@kopin.io>`.

## Key decisions recorded (for Momus)

- **D1** Scoring split: prefix/acronym/recency (mtime) in
  `lixun-index::search`; frecency + query latching in `lixun-daemon`
  post-search. Rationale: matches the data-availability boundary, avoids
  leaking `mtime` through `Hit` / IPC.
- **D2** Per-category gating of prefix boost is **not** implemented in
  Wave A. Prefix boost applies uniformly. Mail's lower category
  multiplier (1.0 vs apps 1.3) partially compensates. Revisit based on
  real-usage feedback after Wave A ships.
- **D3** Frecency decay: Firefox-style bucket weights only. No daily
  `×0.975` multiplicative decay. Decay is purely a function of `now - ts`
  at read time; the stored state is immutable timestamps.
- **D4** Acronym splitter is VSCode-style: a run of 2+ uppercase letters
  collapses to one token with the first CAP as its initial.
  `JSONParser → [JSON, Parser] → JP`, `XMLHttpRequest → XHR`.
- **D5** Top Hit gate: multi-criteria `confidence ≥ 0.6` AND
  `top1_score / top2_score ≥ 1.3`. Single-threshold cliff design is
  explicitly rejected.
- **D6** `[ranking]` config plumbing bug is fixed as task 1 of this wave.
  Category weights today are read from config but then ignored because
  `Category::ranking_boost()` hardcodes them. Task 1 routes config values
  through to the index layer.
- **D7** ClickHistory migration: cold start. Delete the old
  `history.json`, recreate in the new format, lose nothing important
  because the old format only stored counts and all active docs recover
  in a week of normal use.
- **D8** GUI change: add a hero Top Hit region above the scrollable
  results list. The existing `.lixun-top-hit` CSS class stays applied to
  the selected row inside the list (stateful). The hero row gets a new
  class `.lixun-top-hit-hero`. No existing style selectors are renamed.
- **D9** IPC: bump `PROTOCOL_VERSION` to 3. Introduce
  `Response::HitsWithExtrasV3 { hits, calculation, top_hit }`. Daemon
  picks v1 / v2 / v3 response shape based on the negotiated version
  (already tracked per-connection as `negotiated_version`). Relax
  `FrameCodec` to accept any version in `MIN_PROTOCOL_VERSION ..=
  PROTOCOL_VERSION` instead of strict equality.
- **D10** No third-party IPC consumers exist. We can bump without
  external coordination.

## Ranking model

### Signal inventory

| # | Signal | Layer | Fires when | Value |
|---|---|---|---|---|
| 1 | Category multiplier | index | always | `config[ranking.<cat>]` |
| 2 | Prefix boost | index | normalized title starts with normalized query | `prefix_boost` mult |
| 3 | Acronym boost | index | title's initials start with normalized query | `acronym_boost` mult |
| 4 | Recency boost (mtime) | index | category ∈ {File, Mail} and mtime present | `1 + W_recency * exp(-age_days / tau)` |
| 5 | Frecency | daemon | doc has click history | `1 + alpha * normalized_frecency_raw` |
| 6 | Query latch | daemon | normalized query has latched doc_id | `1 + W_latch * ln(1+count) * recency_weight`, capped at `latch_cap` |

### Composition

```
// Stage 1 — inside LixunIndex::search, per Tantivy hit
doc_mult = category_mult(config, category)
         * prefix_mult(title, q, prefix_boost)
         * acronym_mult(title, q, acronym_boost)
         * recency_mult(category, mtime, now, W_recency, tau)
doc_score = tantivy_score * doc_mult

// Stage 2 — inside lixun-daemon, post-sort prep
final_mult = frecency_mult(frecency_state, doc_id, now, alpha)
           * latch_mult(latch_state, q_norm, doc_id, now, W_latch, cap)
final_score = doc_score * min(final_mult, runtime_cap_per_hit / doc_mult)
```

Where:

- Each `*_mult` defaults to 1.0 when the signal does not fire, so setting
  any single weight to its neutral value reproduces pre-existing behavior
  exactly for that signal.
- `total_multiplier_cap = 6.0` is enforced after Stage 2 on the product
  of all non-category multipliers (not on category, since category is
  intentionally always applied).
- Sort is total-order: `partial_cmp` with a NaN-guard, tiebreaker =
  `doc_id` lexicographic (deterministic across runs).

### Normalized query, two functions

Both live in `lixun-index::normalize` (new module) so daemon and index
use the exact same implementation. No inline normalization anywhere.

- `normalize_for_match(q) -> String`: NFKD → strip combining marks →
  ASCII fold → lowercase → trim → collapse internal whitespace to single
  space. Tantivy operators (`-`, `|`, `"`) are preserved.
- `normalize_for_latch_key(q) -> String`: apply `normalize_for_match`,
  then strip leading/trailing `-`, `+`, `"`, collapse remaining `"`,
  preserve word order. Word-order-insensitive collapsing is explicitly
  rejected; `"foo bar"` and `"bar foo"` are different latches.

### Acronym splitter rules (D4)

Input: title string (original case, any Unicode).

1. Split on any `char::is_whitespace`, `_`, `-`, `.`, `/` (unicode-safe).
2. Within each word, split on CamelCase transition rule:
   - `a..A` → split before the capital.
   - `AA..Aa` → split between the last two capitals.
     (i.e. within an all-caps run, only the final CAP starts a new
     subword if followed by a lowercase.)
3. Take the first `char` of each resulting subword, lowercase it, collect
   into a `String`.
4. The acronym boost fires iff `initials.starts_with(normalize_for_match(q))`
   where `initials` is the lowercased initial string.

Unit-test fixtures (required, non-negotiable):

```
"JSONParser"        → "jp"
"XMLHttpRequest"    → "xhr"
"parseURL"          → "pu"
"iPhone"            → "ip"
"snake_case"        → "sc"
"kebab-case"        → "kc"
"Café Pro"          → "cp"
"Firefox"           → "f"
"Google Image Capture" → "gic"
"Visual Studio Code"   → "vsc"
""                  → ""
"  "                → ""
"A"                 → "a"
"ABC"               → "a"   (one all-caps word, one initial)
```

### Frecency model (D3)

Stored state per doc_id:

```rust
struct VisitEntry { ts: i64 /* unix seconds */, kind: VisitKind }
enum VisitKind { Click /* record_click */ }   // extensible later
struct FrecencyRecord {
    visits: VecDeque<VisitEntry>,   // cap 10, oldest popped on push
}
```

Bonus computation (pure, no mutation):

```
fn frecency_raw(record, now) -> f32:
    sum over visits of bucket_weight(now - ts)
fn bucket_weight(age_seconds) -> f32:
    age_days := age_seconds / 86400
    match:
        age_days <=  4 -> 1.00
        age_days <= 14 -> 0.70
        age_days <= 31 -> 0.50
        age_days <= 90 -> 0.30
        otherwise      -> 0.10
fn frecency_mult(record, now, alpha) -> f32:
    raw := frecency_raw(record, now)       // in [0, 10]
    1.0 + alpha * raw
```

Note: because `visits.len() ≤ 10` and each bucket weight ≤ 1.0,
`raw ∈ [0, 10]`. Alpha = 0.1 then gives multiplier in `[1.0, 2.0]`,
which is well-scaled without further normalization.

Persistence: `~/.local/state/lixun/frecency.json`, same atomic write
pattern as today's `history.json`. Cold-start migration (D7): on daemon
start, if `frecency.json` missing and old `history.json` present, delete
the old file and log one line at info level.

### Query latching

Stored state:

```rust
struct LatchEntry { count: u32, last_ts: i64 }
struct QueryLatch {
    // key = normalize_for_latch_key(query)
    map: BTreeMap<String, HashMap<DocId, LatchEntry>>,
}
```

Count is capped at 50 on write (stops unbounded `ln(1+count)` growth).

Bonus:

```
fn latch_mult(latch, q_norm, doc_id, now, w, cap) -> f32:
    entry := latch.map.get(q_norm).and_then(_.get(doc_id))
    entry match:
      None             -> 1.0
      Some(e):
        recency := bucket_weight(now - e.last_ts)   // reuses frecency buckets
        raw := ln(1 + e.count) * recency
        clamp(1.0 + w * raw, 1.0, cap)
```

Lookup is exact-match on `normalize_for_latch_key(q)`. Prefix-range
lookups ("user typed `fo`, had latched `foo`") are out of scope for
Wave A. Rationale: Alfred's latching is exact; prefix-latch introduces
an ordering problem between prefix and frecency that needs its own
tuning.

Recording: new IPC request `Request::RecordQueryClick { doc_id, query }`
(see IPC plan below). The GUI sends both the old `RecordClick` and the
new `RecordQueryClick` for one turn; the daemon accepts both.
`RecordClick` stays for backward compat (old GUI binaries still work,
they just don't learn latches). Persistence:
`~/.local/state/lixun/query_latch.json`.

### Top Hit selection (D5)

After final sort, pick candidate = hits[0]. Compute confidence:

```
confidence := max(
    1.0 if prefix_match(candidate.title, q) else 0.0,
    1.0 if strong_latch(candidate, q, threshold=3) else 0.0,   // count >= 3
    frecency_dominance(candidate, hits),   // in [0, 1], see below
)

margin := candidate.score / max(hits[1].score, epsilon)

top_hit := Some(candidate.id) iff confidence >= 0.6 AND margin >= 1.3
```

`frecency_dominance`: if candidate's frecency_raw ≥ 2× runner-up's
frecency_raw AND candidate's frecency_raw ≥ 3, return 1.0, else 0.0.

### mtime recency

For files, mtime is already stored in the schema (see `lixun-index`
schema: `mtime: STORED (i64)`). Read it during doc retrieval in
`LixunIndex::search` right next to the existing reads of `title` and
`category`. Apply only for `Category::File` and `Category::Mail`.

```
recency_mult(category, mtime, now, w, tau):
    if category not in {File, Mail}: return 1.0
    age_days := (now - mtime) / 86400
    if age_days < 0: age_days = 0            // future-dated files → treat as today
    1.0 + w * exp(-age_days / tau)
```

Defaults: `w=0.2`, `tau=30`. Effect: a doc modified today gets a 1.2×
boost, a 30-day-old doc gets 1.07×, a 1-year-old doc is ~1.00.

## IPC protocol changes

### Constants

- `PROTOCOL_VERSION: u16 = 3` (was 2)
- `MIN_PROTOCOL_VERSION: u16 = 1` (unchanged)

### FrameCodec

Replace strict equality with a range check:

```rust
if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version) {
    return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("version {} outside supported {}..={}", version,
                MIN_PROTOCOL_VERSION, PROTOCOL_VERSION)));
}
```

This makes the codec a passthrough for any version the daemon can speak.
The per-connection handshake (daemon-side manual peek; see
`lixun-daemon/src/main.rs` around lines 425–434) is unchanged and
remains the authoritative `negotiated_version` source.

### Request (additions)

```rust
// New; sent by Wave A GUI alongside the existing RecordClick.
RecordQueryClick { doc_id: String, query: String },
```

The existing `RecordClick { doc_id }` is preserved. A v3 GUI emits
`RecordQueryClick` for latching AND continues to emit `RecordClick` so
frecency accounting is identical for v2-fallback paths.

### Response (additions)

```rust
HitsWithExtrasV3 {
    hits: Vec<Hit>,
    calculation: Option<Calculation>,
    top_hit: Option<DocId>,
}
```

The daemon picks the response shape from `negotiated_version`:

| negotiated | shape |
|---|---|
| 1 | `Hits(Vec<Hit>)` |
| 2 | `HitsWithExtras { hits, calculation }` |
| 3 | `HitsWithExtrasV3 { hits, calculation, top_hit }` |

### GUI

- v3 GUI sends `RecordQueryClick` every time a row is opened, carrying
  the current entry-box text at click time.
- v3 GUI reads `top_hit` from the response. If `Some(doc_id)` and that
  doc is in `hits`, render it in the new `.lixun-top-hit-hero` region
  above the list; otherwise hide the region.

### CLI

The in-tree `lixun-cli` (`crates/lixun-cli/src/main.rs`) rebuilds
against the same workspace `PROTOCOL_VERSION`, so after the bump it
becomes a v3 client by virtue of linking. Its `handle_response` match
(currently covers only `Hits` and `HitsWithExtras`) gains a
`HitsWithExtrasV3` arm that prints hits + calculation identically to
the v2 arm and ignores `top_hit` (CLI is a text dump; Top Hit is a
visual concept). Without this arm, `cargo run -p lixun-cli -- search`
fails at `serde_json::from_slice` with an unknown-variant error
against a live v3 daemon. Covered by QA scenario 5 of Task 5.

## Config surface

New keys in `[ranking]`. Existing category keys are preserved and
*actually start working* after task 1.

```toml
[ranking]
# Category multipliers (existing, but previously ignored)
apps        = 1.3
files       = 1.2
mail        = 1.0
attachments = 0.9

# New in Wave A
prefix_boost         = 1.4
acronym_boost        = 1.25
recency_weight       = 0.2
recency_tau_days     = 30.0
frecency_alpha       = 0.1
latch_weight         = 0.5
latch_cap            = 3.0
total_multiplier_cap = 6.0

# Top Hit selection
top_hit_min_confidence = 0.6
top_hit_min_margin     = 1.3
```

Setting any multiplicative weight to 1.0 (or any alpha to 0.0, or the
cap to 1.0) neutralizes its signal without code changes.
`docs/config.example.toml` documents each key; the README gets a short
cross-reference.

## Tasks

Tasks are strictly ordered. Each ends with a green `cargo test
--workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.
No task is merged until both pass.

### Task 1 — fix `[ranking]` config plumbing (D6)

Implementation:

- Introduce `RankingConfig` in `lixun-core` (not `lixun-daemon::config`,
  because `lixun-index` must depend on it and must not pull in daemon
  crates) as a `Clone` struct with category fields (`apps`, `files`,
  `mail`, `attachments`) plus all Wave A weights added in tasks 3, 4, 5
  (leave those fields behind `Default::default()` stubs for this task;
  later tasks fill them in). Reference: config surface block above.
- Widen the two real constructors
  `LixunIndex::create_or_open(index_path: &str)` (`lixun-index/src/lib.rs:128`)
  and `LixunIndex::create_or_open_with_plugins(index_path: &str, plugins: CompiledPluginSchema)`
  (`lixun-index/src/lib.rs:140`) to take an additional
  `ranking: RankingConfig` argument. Store it in the `LixunIndex`
  struct. All existing call sites (daemon startup, tests) are updated
  to pass the ranking config they build from TOML.
- Replace the expression at `lixun-index/src/lib.rs:343`,
  `score: score * category.ranking_boost()`, with
  `score: score * self.ranking.multiplier_for(category)`.
- Delete `Category::ranking_boost()` from `lixun-core` (lines 25-32)
  and its unit test `test_category_ranking_boost` (lines 204-210).
  Verify no other caller: `rg 'ranking_boost' crates/` returns zero
  results before commit.
- In `lixun-daemon/src/config.rs` wire the existing TOML fields
  (`ranking_apps` etc.) into the new `RankingConfig` builder.

QA scenario (automated, exit criterion):

- Command: `cargo test -p lixun-index test_ranking_config_category_multiplier`
- Test lives in `crates/lixun-index/src/lib.rs` `#[cfg(test)] mod tests`.
- Steps in the test: build two docs with identical title "zzz" and
  identical lexical score, one `Category::App`, one `Category::File`.
  Construct `LixunIndex::create_or_open_with_plugins` with
  `RankingConfig { apps: 99.0, files: 1.0, mail: 1.0, attachments: 1.0, ..Default::default() }`.
  Call `search(Query{ text: "zzz", limit: 10 })`.
- Expected: `hits[0].category == Category::App`, `hits[1].category == Category::File`,
  `hits[0].score > hits[1].score * 90.0`.

- Regression command: `cargo test --workspace` — must stay green, and
  `cargo clippy --workspace --all-targets -- -D warnings` must stay clean.

### Task 2 — `lixun-index::normalize` module

Implementation:

- New file `crates/lixun-index/src/normalize.rs`, referenced from
  `crates/lixun-index/src/lib.rs` via `pub mod normalize;`.
- Export `normalize_for_match(&str) -> String` and
  `normalize_for_latch_key(&str) -> String`.
- Depend on the `unicode-normalization` crate (add to workspace deps
  in the root `Cargo.toml`); implement combining-mark stripping by
  filtering out chars in the `[\u{0300}-\u{036f}]` range after NFKD.
- Pure functions, no state, no config, no IO.

QA scenario (automated, exit criterion):

- Command: `cargo test -p lixun-index normalize::tests`
- Test fixtures (data-driven, one `#[test]` per function with a table):

  `normalize_for_match` fixtures:
  | input | expected |
  |---|---|
  | `""` | `""` |
  | `"   "` | `""` |
  | `"Café"` | `"cafe"` |
  | `"RÉSUMÉ"` | `"resume"` |
  | `"Foo   Bar"` | `"foo bar"` |
  | `"-foo"` | `"-foo"` |
  | `"naïve"` | `"naive"` |
  | `"日本語"` | `"日本語"` |

  `normalize_for_latch_key` fixtures:
  | input | expected |
  |---|---|
  | `""` | `""` |
  | `"-foo"` | `"foo"` |
  | `"+foo"` | `"foo"` |
  | `"\"foo bar\""` | `"foo bar"` |
  | `"foo bar"` | `"foo bar"` |
  | `"bar foo"` | `"bar foo"` |
  | `"Café"` | `"cafe"` |

- Expected: every row passes with `assert_eq!`.
- Regression: `cargo clippy --workspace --all-targets -- -D warnings`
  clean.

### Task 3 — prefix + acronym + mtime recency in `LixunIndex::search`

Implementation:

- New module `crates/lixun-index/src/scoring.rs` with three pure
  functions:
  - `prefix_mult(title: &str, q_norm: &str, weight: f32) -> f32`
  - `acronym_mult(title: &str, q_norm: &str, weight: f32) -> f32`
    (splitter per D4)
  - `recency_mult(category: Category, mtime_secs: i64, now_secs: i64, weight: f32, tau_days: f32) -> f32`
  Each returns `1.0` when the signal does not fire.
- Extend `LixunIndex::search` at `crates/lixun-index/src/lib.rs:274`:
  read `mtime` from the stored `TantivyDocument` alongside the existing
  reads of `title` / `category`, compute
  `doc_mult = category * prefix * acronym * recency`, apply it to the
  Tantivy score before emitting the `Hit`.
- Widen `RankingConfig` (populated stub-only in task 1) with
  `prefix_boost`, `acronym_boost`, `recency_weight`, `recency_tau_days`.
- Wire the new TOML keys through `lixun-daemon/src/config.rs`.

QA scenarios (automated, exit criterion — all three must pass):

1. Command: `cargo test -p lixun-index scoring::tests::acronym_fixtures`
   Fixtures (D4 table verbatim): assert `acronym_initials("JSONParser") == "jp"`,
   `"XMLHttpRequest" == "xhr"`, `"Café Pro" == "cp"`, `"ABC" == "a"`,
   `"" == ""` — 14 rows total.

2. Command: `cargo test -p lixun-index scoring::tests::prefix_and_unicode`
   Assert `prefix_mult("Firefox", "fire", 1.4) == 1.4`,
   `prefix_mult("campfire", "fire", 1.4) == 1.0`,
   `prefix_mult("Café Pro", "caf", 1.4) == 1.4`
   (`q_norm` passed through `normalize_for_match` already, so "caf"
   matches title "café" after fold).

3. Command: `cargo test -p lixun-index scoring::tests::recency_orders_ties`
   Steps: build index with two `Category::File` docs, identical
   title + body, different `mtime` (doc-A now, doc-B now-60d).
   Search query matching both. Assert doc-A ranks above doc-B.
   Also assert: same test with `Category::App` instead of File → order
   is unchanged (recency not applied).

4. Regression: with every new weight set to its neutral value
   (`prefix_boost=1.0, acronym_boost=1.0, recency_weight=0.0`),
   `cargo test --workspace` produces identical output to a pre-task-3
   baseline recorded in the commit. (Verified via SC-23..SC-25 being
   gated on defaults rather than neutrals; see Task 8.)

### Task 4 — replace `ClickHistory` with `Frecency`

Implementation:

- New file `crates/lixun-daemon/src/frecency.rs`. Define
  `FrecencyStore` with methods `record_click(&mut self, doc_id: &str, now: i64)`,
  `mult(&self, doc_id: &str, now: i64, alpha: f32) -> f32`,
  `raw(&self, doc_id: &str, now: i64) -> f32`,
  `load(state_dir: &Path) -> Result<Self>`,
  `save(&self, state_dir: &Path) -> Result<()>`.
  Storage file: `frecency.json` in `state_dir`.
- Delete `crates/lixun-daemon/src/history.rs` and its module
  declaration in `main.rs`.
- At `crates/lixun-daemon/src/main.rs:446-448` replace the additive
  loop `hit.score += history.bonus(&hit.id.0)` with a multiplicative
  loop `hit.score *= frecency.mult(&hit.id.0, now, alpha)`.
- Cold-start migration inside `FrecencyStore::load`: if
  `state_dir.join("history.json")` exists, delete it with one
  `tracing::info!` line ("migrated from ClickHistory: cold start,
  frecency empty"). No attempt to port counts.

QA scenarios (automated, exit criterion — all four must pass):

1. Command: `cargo test -p lixun-daemon frecency::tests::bucket_weights`
   Fixtures: `bucket_weight(1 * DAY) == 1.00`,
   `bucket_weight(10 * DAY) == 0.70`, `bucket_weight(25 * DAY) == 0.50`,
   `bucket_weight(60 * DAY) == 0.30`, `bucket_weight(365 * DAY) == 0.10`.

2. Command: `cargo test -p lixun-daemon frecency::tests::mult_semantics`
   Steps: empty store → `mult("foo", now, 0.1) == 1.0`; record 3 clicks
   for "foo" at `now`; `mult("foo", now, 0.1)` returns `1.0 + 0.1 * 3.0 == 1.3`
   (within `f32::EPSILON * 10`).

3. Command: `cargo test -p lixun-daemon frecency::tests::save_load_roundtrip`
   Tempdir-based; record 5 clicks across 3 doc_ids, save, drop, load
   fresh instance, assert `mult` values identical to pre-save.

4. Command: `cargo test -p lixun-daemon frecency::tests::cold_start_migration`
   Tempdir; write a bogus `history.json` with content `{"counts":{"x":5}}`;
   call `FrecencyStore::load(tempdir)`; assert the old file no longer
   exists and the returned store's `mult("x", now, 0.1) == 1.0`.

Regression: `cargo test --workspace` green (ClickHistory usages fully
migrated; no stale refs).

### Task 5 — `QueryLatch` + protocol v3 (IPC + CLI update)

Implementation:

- New file `crates/lixun-daemon/src/query_latch.rs`. Define
  `QueryLatchStore` with
  `record(&mut self, query_raw: &str, doc_id: &str, now: i64)`
  (internally applies `normalize_for_latch_key`),
  `mult(&self, query_raw: &str, doc_id: &str, now: i64, weight: f32, cap: f32) -> f32`,
  `strong(&self, query_raw: &str, doc_id: &str, threshold: u32) -> bool`,
  `load(state_dir)`, `save(state_dir)`.
  Storage file: `query_latch.json` in `state_dir`. Count capped at
  50 on write.
- In `crates/lixun-ipc/src/lib.rs`:
  - Bump `PROTOCOL_VERSION` from 2 to 3.
  - Add `Request::RecordQueryClick { doc_id: String, query: String }`.
  - Add `Response::HitsWithExtrasV3 { hits: Vec<Hit>, calculation: Option<Calculation>, top_hit: Option<DocId> }`
    (Top Hit wiring completes in Task 6; the variant is introduced
    here to ship the codec change atomically).
  - Relax `FrameCodec` decoder version check (currently at
    `crates/lixun-ipc/src/lib.rs:165-172`): from strict
    `!= PROTOCOL_VERSION` to range check
    `!(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)`.
    Error message updated accordingly.
- In `crates/lixun-cli/src/main.rs`:
  - Add a `Response::HitsWithExtrasV3 { hits, calculation, top_hit }`
    arm to `handle_response` at line 110. Ignore `top_hit` in printed
    output (CLI is a text dump; Top Hit is a GUI concept). Render
    `hits` and optionally `calculation` exactly as the v2 arm does.
  - No other CLI changes required. The CLI will automatically become
    a v3 client because it rebuilds against the updated
    `PROTOCOL_VERSION` constant.
- In `crates/lixun-daemon/src/main.rs::handle_client`:
  - Handle `Request::RecordQueryClick` by updating both the query log
    (existing behavior, mirrored from `Request::RecordQuery`) and the
    new latch store.
  - In the `Request::Search` arm, after the frecency-mult loop, apply
    `latch.mult(&q, &hit.id.0, now, W_LATCH, LATCH_CAP)`.
- `RankingConfig` gets `frecency_alpha`, `latch_weight`, `latch_cap`,
  `total_multiplier_cap`; TOML keys wired through.

QA scenarios (automated, exit criterion — all five must pass):

1. Command: `cargo test -p lixun-ipc test_protocol_v3_record_query_click_roundtrip`
   In `crates/lixun-ipc/src/lib.rs` tests: encode
   `Request::RecordQueryClick { doc_id: "fs:/a".into(), query: "foo".into() }`,
   decode, assert fields match.

2. Command: `cargo test -p lixun-ipc test_codec_accepts_protocol_v2_frame`
   Manually craft a frame with `version=2` and a `Request::Toggle`
   payload (matching `FrameCodec`'s `Decoder::Item = Request`); assert
   `FrameCodec::decode` does NOT error on version=2 and returns
   `Some(Request::Toggle)`. This is the v2-backward-compat regression
   guard for the codec itself. Response-shape compat is covered
   separately by `lixun-daemon top_hit::tests::v2_response_shape_preserved`
   (Task 6 QA scenario 3).

3. Command: `cargo test -p lixun-daemon query_latch::tests::cap_and_ordering`
   Steps: fresh store; record 5 clicks for `("foo", "doc1")`; assert
   `mult("foo", "doc1", now, 0.5, 3.0) > 1.0 && <= 3.0`;
   record 5 more (count cap to 50 irrelevant here); assert mult still
   `<= 3.0` (cap respected). Assert `mult("foo", "doc2", ...) == 1.0`
   (unrelated doc unaffected).

4. Command: `cargo test -p lixun-daemon query_latch::tests::word_order_matters`
   Record `("foo bar", "doc1", now)`; assert
   `mult("bar foo", "doc1", now, ...) == 1.0` and
   `mult("foo bar", "doc1", now, ...) > 1.0`.

5. Command: `cargo build --workspace && ./target/debug/lixun search foo`
   (integration-style; requires daemon running from the same tree).
   Expected: no `serde` error, hits printed. This exercises the v3
   negotiation path from the in-tree CLI end-to-end.

Regression: `cargo test --workspace` green; `cargo clippy
--workspace --all-targets -- -D warnings` clean.

### Task 6 — Top Hit backend + version-gated response shape

Implementation:

- `Response::HitsWithExtrasV3` is already defined in Task 5.
- In `crates/lixun-daemon/src/main.rs::handle_client`, the `Search`
  arm currently branches on `if negotiated_version >= 2` (line 455).
  Extend to three-way:
  ```
  match negotiated_version {
      1 => Response::Hits(hits),
      2 => Response::HitsWithExtras { hits, calculation },
      _ => Response::HitsWithExtrasV3 { hits, calculation, top_hit },
  }
  ```
- Compute `top_hit` after the Stage 2 sort per D5: build `confidence`,
  compute `margin = hits[0].score / max(hits[1].score, EPSILON)`,
  return `Some(hits[0].id.clone())` iff both thresholds satisfied,
  else `None`. When `hits.len() <= 1`, treat `margin` as `INF` (the
  only hit is its own Top Hit if confidence threshold met).
- `frecency_dominance(candidate, hits)` helper lives in the same
  module; pure function; uses `FrecencyStore::raw`.
- `RankingConfig` gets `top_hit_min_confidence` (default 0.6) and
  `top_hit_min_margin` (default 1.3); TOML keys wired through.

QA scenarios (automated, exit criterion — all four must pass):

1. Command: `cargo test -p lixun-daemon top_hit::tests::prefix_match_sets_top_hit`
   Build a small Tantivy index in-memory (reuse Task 7 helper if it
   exists yet, otherwise inline here and migrate later). Search with
   a query that prefix-matches exactly one doc; assert response is
   `HitsWithExtrasV3` with `top_hit == Some(that_doc_id)`.

2. Command: `cargo test -p lixun-daemon top_hit::tests::ambiguous_returns_none`
   Two docs with identical title and identical frecency; search with
   a body-only query; assert `top_hit == None` (margin < 1.3).

3. Command: `cargo test -p lixun-daemon top_hit::tests::v2_response_shape_preserved`
   Simulate `negotiated_version = 2` path; assert response variant is
   `HitsWithExtras`, NOT `HitsWithExtrasV3`. This is the backward
   compat guard requested by Momus.

4. Command: `cargo test -p lixun-daemon top_hit::tests::v1_response_shape_preserved`
   Simulate `negotiated_version = 1`; assert `Response::Hits(..)`.

Regression: `cargo test --workspace` green.

### Task 7 — GUI hero region + latch recording

Implementation:

- In `crates/lixun-gui/src/window.rs`, add a hero container widget
  above the existing results list container. New CSS class
  `.lixun-top-hit-hero`. Shown iff the response is
  `HitsWithExtrasV3 { top_hit: Some(id), .. }` and `id` is present in
  `hits`.
- In `crates/lixun-gui/src/factory.rs`, when the user opens a row,
  dispatch two requests: the existing `Request::RecordClick { doc_id }`
  (preserved for frecency), AND a new
  `Request::RecordQueryClick { doc_id, query: entry.text().into() }`
  (drives latching). Both are fire-and-forget.
- In `crates/lixun-gui/style.css`, add a `.lixun-top-hit-hero` rule
  duplicating the card-elevation properties currently on
  `.lixun-top-hit` (lines 111-141). Do not rename or remove the
  existing class.
- `docs/style.example.css` gets the new selector with a one-line
  comment that this is the structural hero-row class vs. the stateful
  selected-row class.

QA scenarios (a mix of automated + one manual walkthrough):

1. Automated — command: `cargo test -p lixun-gui window::tests::response_routing_renders_hero`
   Mock the IPC response with `HitsWithExtrasV3 { top_hit: Some(id), ..}`;
   invoke the GUI's response handler in headless mode (GTK test
   harness `gtk::test_init()`); assert the hero container's child
   count is 1 after `render_response(..)`.

2. Automated — command: `cargo test -p lixun-gui window::tests::hero_hidden_without_top_hit`
   Same harness; mock response with `top_hit: None`; assert hero
   container child count is 0.

3. Automated — command: `cargo test -p lixun-gui factory::tests::click_emits_both_requests`
   Inject a mock IPC client, trigger a row click, assert both
   `RecordClick` and `RecordQueryClick` are sent.

4. Manual QA walkthrough (added to `docs/QA-spotlight-parity.md`
   Manual section as a new bullet SC-GUI-HERO):
   - Steps: start daemon fresh. Type `firefox`. Expected: row labelled
     Firefox appears as a visually-distinct hero row above the list;
     CSS class `.lixun-top-hit-hero` is applied (verify with
     `GTK_DEBUG=interactive lixun-gui`).
   - Steps: type `zzzzz`. Expected: hero region hidden; status bar
     shows "No results".
   - Steps: type `fo`, select doc X, press Enter; re-open launcher
     and type `fo` again. Expected: doc X ranks higher than on the
     first search (latch learned). Repeat 3× to saturate.

### Task 8 — QA-parity doc + new SCs

Implementation: update `docs/QA-spotlight-parity.md` to add the SC
rows below, each citing the concrete `cargo test` target that covers
it (so any reviewer can trace SC → test).

| SC  | Criterion                                            | Covered by                                                    |
|-----|-------------------------------------------------------|---------------------------------------------------------------|
| 23  | Prefix boost: "fire" ranks Firefox above unrelated    | `lixun-index scoring::tests::prefix_and_unicode`              |
| 24  | Acronym match: "vsc" ranks Visual Studio Code         | `lixun-index scoring::tests::acronym_fixtures`                |
| 25  | Recency: newer mtime wins for tied files              | `lixun-index scoring::tests::recency_orders_ties`             |
| 26  | Frecency replaces additive bonus (mult semantics)     | `lixun-daemon frecency::tests::mult_semantics`                |
| 27  | Query latching pins doc for its query                 | `lixun-daemon query_latch::tests::cap_and_ordering`           |
| 28  | Top Hit present iff confidence + margin satisfied     | `lixun-daemon top_hit::tests::prefix_match_sets_top_hit` + `ambiguous_returns_none` |
| 29  | Protocol v2 clients still work                        | `lixun-ipc test_codec_accepts_protocol_v2_frame` + `lixun-daemon top_hit::tests::v2_response_shape_preserved` |
| 30  | `[ranking]` config values apply (guards D6 fix)       | `lixun-index test_ranking_config_category_multiplier`         |

Manual SCs appended to the existing "Manual" block:

- `SC-GUI-HERO` hero row renders with `.lixun-top-hit-hero` (Task 7
  walkthrough).

QA scenario (exit criterion of this task):

- Command: `grep -Ec '^\| (2[3-9]|30) ' docs/QA-spotlight-parity.md`
  returns exactly `8`. The anchor `^\| ` followed by the numeric range
  `(2[3-9]|30)` and a trailing space matches exactly the eight new
  rows (SC-23..SC-30) in the automated table. SC-GUI-HERO lives in
  the Manual block and is intentionally not matched by this regex.
- Command: `cargo test --workspace` green.
- Command: `cargo clippy --workspace --all-targets -- -D warnings`
  clean.

## Risks

These go into the plan's permanent "Risks" section. Each has a
mitigation.

1. **Signal composition order is load-bearing.** Mitigation: locked by
   D1 and the pseudocode in Composition. Any deviation is a test
   failure by design.
2. **Multiplier saturation can invert BM25 body relevance.** Mitigation:
   `total_multiplier_cap = 6.0` bounds the product; SC-3 (fuzzy
   "chrm"→"Chrome") is a regression guard.
3. **Lazy-decay under read-lock would deadlock or thrash.** Mitigation:
   frecency bonus is a pure function of `(record, now)` with no
   mutation. State changes happen only in `record_click` under a write
   lock.
4. **NaN in `partial_cmp` produces non-deterministic sort.** Mitigation:
   replace `sort_by` with a total-order comparator that maps NaN → +∞
   (pushed to the bottom) and tiebreaks on `doc_id`.
5. **Test isolation**: category boost silently makes signal tests pass.
   Mitigation: every Wave A test either (a) uses the same category for
   all fixtures, or (b) passes a `RankingConfig` with all category
   multipliers = 1.0. Test doctrine written into `tests/README.md` (new
   file).
6. **Protocol negotiation happens via manual peek, not codec**.
   Mitigation: document it in the `lixun-ipc` module doc, relax codec
   to range-check (task 5), add an explicit test that crosses the
   manual-peek boundary.
7. **History migration synthetic timestamp biases Top Hit for a week
   post-upgrade.** Mitigation: cold-start (D7) accepts a one-week UX
   dip in exchange for correctness. Users with active docs see them
   resurface within days. CHANGELOG calls this out.
8. **Normalized-query drift across write/read/match paths** is the
   single most likely cross-signal bug. Mitigation: D-rule enforced by
   putting both normalize functions in one module, used by everyone, no
   inline variants allowed. Grep-based regression check: no call to
   `.to_lowercase()` on query strings outside `normalize.rs`.
9. **GUI `.lixun-top-hit` semantic change may conflict with existing
   row-selection CSS.** Mitigation: new class
   `.lixun-top-hit-hero`, old class untouched (D8). No existing theme
   in `docs/style.example.css` is broken; one new block added.
10. **Prefix boost on mail is a latent UX issue** for users who use
    the launcher heavily for email. Mitigation: D2 accepts this for
    Wave A; category multiplier `mail = 1.0` (vs apps `1.3`) partially
    compensates. Revisit with a Wave A2 ticket if feedback warrants.
11. **`frecency_raw` scale is bounded by design** (sample cap = 10,
    max bucket = 1.0, so `raw ≤ 10`). Mitigation: documented in the
    module, tested with a saturation fixture.
12. **Acronym splitter Unicode edge cases.** Mitigation: D4 test
    fixtures include `"Café Pro"`. Implementation iterates over
    `char_indices()`, never over bytes.

## Out-of-scope, cross-referenced

- Calculator-as-plugin: separate plan (next wave after Wave A lands).
- Shell-exec plugin: separate plan.
- OCR: separate plan.
- Semantic search: separate plan.
- Per-category signal gating (e.g. prefix off for mail): deferred, see
  D2.
- Prefix-range latch lookup: deferred, see "Query latching" section.

## Success criteria (plan-level)

The plan is successful when:

- All tasks 1–8 merge, each with its tests passing in the commit that
  adds it.
- `cargo test --workspace` green; `cargo clippy --workspace
  --all-targets -- -D warnings` green.
- `docs/QA-spotlight-parity.md` updated; SC-1..SC-22 still green;
  SC-23..SC-30 added and green (automated).
- Manual QA walk (docs/QA-spotlight-parity.md Manual section) passes
  the new `SC-GUI-HERO` entry.
- The in-tree CLI rebuilt at v3 works end-to-end against a Wave-A
  daemon (covered by Task 5 QA scenario 5).
- An externally-built binary of `lixun-cli` at v2 (e.g. from the last
  release tarball) can still talk to the Wave-A daemon; the daemon's
  `handle_client` dispatches `Response::HitsWithExtras` for that
  connection. Covered by `lixun-daemon top_hit::tests::v2_response_shape_preserved`
  and `lixun-ipc test_codec_accepts_protocol_v2_frame`.

## Momus checklist (self-review before handoff)

- [x] Every signal has a defined neutral value that disables it.
- [x] All configurable weights listed in one place with defaults.
- [x] Each task has an exit criterion that is checkable (`cargo test`
      / specific assertion).
- [x] No task is "do the GUI" without a measurable outcome.
- [x] Risks enumerate the 10 red flags from Metis plus 2 additions.
- [x] AGENTS.md invariants respected: no host branches on plugin id,
      no domain-specific code in host, no AI-agent names anywhere.
- [x] Protocol change documented end-to-end (codec, request, response,
      client behavior).
- [x] Migration path (D7) explicitly described.
- [x] Out-of-scope items listed with pointers to future plans.

