# Roadmap: standing v2, the message archive, and Hugh Mind

This is the plan for Hugh's next major version. It replaces one-size-fits-all
automod limits with limits fitted to each member, backed by a message archive
and a model Hugh trains on its own server's activity.

## Deployment assumptions

- Hugh and its trainer run on one home server with a GTX 1060 attached. The
  6 GB card is the target; every GPU job has a 3 GB profile as well.
- Around 5,000 messages a day, about 1.8 million a year.
- A storage budget of 70 GB under `data/`.

At this volume one message costs about 0.4 KB, counting its text, features,
embedding and index entries. A year comes to roughly 0.7 GB, so the 70 GB
budget holds decades of history. Retention is therefore a privacy choice
(`archive.retention_days`) rather than something disk space forces, and SQLite
handles the whole archive. Parquet and a separate columnar store are not
needed at this scale.

| Store | Format | Size per year | Ceiling |
|---|---|---|---|
| Message archive: text, edits, deletions, features | SQLite (WAL) `data/archive.sqlite3` | ~0.35 GB | 30 GB |
| Text embeddings: 384-dim int8 | SQLite blob, written by the trainer | ~0.7 GB | 20 GB |
| Full-text and vector indexes | tantivy and HNSW under `data/index/` | ~0.3 GB | 6 GB |
| Live member profiles | redb `data/profiles.redb` | under 20 MB | 1 GB |
| Model registry: every checkpoint and ONNX export | files under `data/models/` | ~2 GB | 8 GB |
| Training scratch | files | — | 3 GB |
| Headroom for compaction and backups | — | — | 2 GB |

`archive.max_bytes` caps the archive. Once the archive passes the cap, Hugh
keeps recording features and evidence but stops storing the text of messages
that no case refers to.

## Components

```text
Discord ─► bot.rs ─► features.rs (pure) ──► archive.rs (SQLite writer thread)
                │                                   │
                ├─► profile.rs (live baselines)     │  read-only
                ├─► mind.rs (CPU inference, ONNX,   ▼
                │   hot-swapped, rules fallback) ◄── mind/ (Python, PyTorch, GPU)
                └─► standing v2 ─► decision ─► enforcement, cases, review queue
```

- `features.rs` turns a message into a fixed, versioned vector of about 24
  numbers. It is pure, and both the live baselines and the model read it.
- `archive.rs` owns `data/archive.sqlite3`. One writer thread batches inserts so
  the gateway never waits on disk. Deletes and edits travel through the same
  queue, so a delete can never overtake the insert it refers to.
- `profile.rs` holds each member's live baselines.
- `mind.rs` loads `data/models/current.onnx` with `tract` and swaps it in when
  the file changes. With no model file present, Hugh behaves exactly as it does
  today.
- `mind/` is the Python trainer. It reads the archive read-only and writes
  versioned models back.

## Inference tiers

1. **Tier 0**: inside the bot, synchronous, under 1 ms. It runs the rules, the
   live baselines and a small feature MLP, and decides whether a message is
   deleted on the spot. It works with the GPU job stopped.
2. **Tier 1**: the GPU service on the same machine, asynchronous, target p95
   under 300 ms. It runs the text encoder, the member sequence model and the
   relationship graph. It can act after the fact: delete the message, time the
   member out, or open a review item. When it misses its deadline, Tier 0's
   decision stands.

## Hugh Mind

### Layer A: live baselines (Rust, no GPU)

Every style feature keeps per member:

- a running mean and variance (Welford)
- a fast and a slow EWMA
- P² estimates of the p50 and p95

Each baseline is shrunk toward the server average by `n / (n + k)`, so a new
account is judged like everyone else and a regular is judged against
themselves. Baselines learn only from messages that tripped no guard, and each
can move only a limited amount per day, so nobody can train Hugh into
tolerating them.

The **personal allowance** for a ratio guard such as caps is the limit raised
toward the member's own p95, clamped to 0.8x–2.0x of the configured value.
Someone who always types in capitals gets room. Someone who never does keeps
the base limit, and a sudden burst from them is an anomaly that feeds risk.

### Layer B: the network (PyTorch on the 1060, fp32)

- **Text encoder**: multilingual MiniLM-L12 on the 6 GB card, MiniLM-L6 on the
  3 GB card. It starts from public toxicity data, then keeps pretraining on the
  archive (masked LM) so it learns the server's own slang. This step needs no
  labels.
- **Member sequence model**: a 4-layer Transformer over each member's last 128
  messages, taking the text embedding, the features and the time gaps. It is
  conditioned on a learned 64-dim **member embedding**.
- **Relationship graph**: edges come from replies, mentions and shared
  conversations. Each edge has a learned pair embedding, so banter between
  friends and the same words aimed at a stranger score differently.
- **Output heads**:
  - probability per violation category, including harassment that knows its
    target
  - a self-supervised "unusual for this person" score, from the error in
    predicting the next message
  - what a moderator would do
  - account takeover
  - a contrastive writing-style fingerprint, used for alt detection
- **Labels**:
  - an automod case later reversed with `/unwarn` counts as a false positive
  - a moderator action within ten minutes of a message counts as a positive
  - the ✅ / ❌ buttons on automod case cards
  - long clean stretches count as negatives
- **Areas of interest**: interest markers point at archived messages, for
  example where a member's style shifted, their first link, a change in tone
  toward someone, or a burst outside their usual hours. Attention weights say
  which messages drove a score.

### Training cadence

| Job | Interval | 6 GB profile | 3 GB profile |
|---|---|---|---|
| Fine-tune on new data, plus updated member embeddings | 15 min | batch 64 | batch 16, gradient accumulation ×4 |
| Continued pretraining of the text encoder | nightly | L12, sequence 128 | L6, sequence 96, gradient checkpointing |
| Full sequence-model retrain and index rebuild | nightly | — | — |
| Writing-style fingerprint model | weekly | — | — |

Live inference keeps about 400 MB of VRAM resident. The trainer pauses between
batches when the inference queue backs up, and on the 3 GB profile it hands the
card back entirely while a large job runs.

A candidate model is promoted only when it beats the current one on held-out
precision and false-positive rate. The last 50 versions are kept, so
`/mind rollback` is instant.

At 1.8 million messages a year the training data is modest. Per-member
embeddings are only trained for members with at least 200 messages, and the
rest rely on the server average and Layer A.

## Standing v2

- **Record score**: punishments decay exponentially by half-life, and repeat
  offences in the same category decay more slowly.
- **Trust score**: earned through tenure, active days and clean messages. It is
  capped and grows slowly.
- **Per-category strictness**: spam, style, links, mentions and scams each have
  their own multiplier, so a spam history does not tighten the caps guard.
- **Effective limit** = base × band multiplier for the category × personal
  allowance × risk adjustment.
- **Graded detectors**: every detector returns a score and a confidence.
  Borderline cases go to a review queue instead of being deleted.

### Hard limits on the model

- The model can never relax the invite, blocked-domain, blocked-phrase,
  @everyone, protected-role or coordinated-spam guards.
- The model can never kick or ban anyone. It may extend a timeout and flag a
  member for review.
- Every new detector runs in shadow mode first, and a weekly report compares
  what it would have done with the live rules.

## Commands

| Phase | Commands |
|---|---|
| 1 | `/mute`, `/unmute`, `/softban`, `/purge`, `/slowmode`, `/lockdown`, `/unlockdown`, `/raid`, `/note`, `/case`, `/reason`, `/watch`, `/nick`, `/dehoist`; message context menus "Report to staff" and "Warn"; ban appeals |
| 1 | `/messages`, `/deleted`, `/context`, plus the offending message shown on its case card |
| 2 | `/profile`, `/search` |
| 3 | `/mind status`, `/mind train`, `/mind rollback`, `/privacy`, `/privacy export`, `/privacy optout`, `/privacy delete` |
| 4 | `/similar`, `/relationships` |
| 5 | `/alts`, `/trend` |

Commands that read the archive (`/messages`, `/deleted`, `/search`, `/alts`)
default to `admin`, and every use is filed as a case, the same way `/scrape`
is.

## Build order

0. **Groundwork**:
   - `features.rs` (done)
   - `archive.rs` (done)
   - the message log (done)
   - `/mute` and `/unmute` (done)
   - per-category strictness
1. **Standing v2 and the Phase 1 commands**, with evidence on case cards.
2. **Live baselines**: `profile.rs`, `/profile`, full-text search.
3. **The trainer and Tier 1 in shadow mode**:
   - `mind/` and `mind.rs` (done; see [mind/README.md](../mind/README.md))
   - the feedback buttons (done)
   - `/mind` and `/privacy`

   As built, the member sequence model reads the author's last 32 messages and
   the channel's last 16 in one transformer. The channel window does part of
   the relationship graph's job: it shows who a message answers and whether
   the author is in the conversation. A learned graph over pairs of members
   stays in phase 4.
4. **Tier 1 live**:
   - allowances and risk switched on for the style guards, once shadow data
     shows fewer false positives than the rules alone
   - the relationship graph
5. **Advanced detection**:
   - alt detection
   - account takeover
   - scam-wave matching (text and image)
   - Unicode confusable skeletons for phrases and domains
   - raid clustering

## Privacy and security

- The archive stores message text. `docs/security.md` states what is kept, for
  how long, and who can read it.
- `data/` belongs on an encrypted disk, readable only by the Hugh service
  account.
- `/privacy delete` and deleted Discord accounts write an erasure tombstone.
  The archive drops that member's rows straight away, their member embedding is
  removed, and the next full retrain excludes them.
- Discord's Developer Policy limits training models on message content.
  Training is therefore opt-out per member and can be turned off for the server
  with `[mind] train_on_content = false`. In that mode the model learns from
  features only.
