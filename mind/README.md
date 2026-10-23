# Hugh Mind

The model Hugh trains on its own server's history, and the scorer Hugh calls
for every new message. It runs beside Hugh on the home server and uses the
GPU there.

## What it learns

Every message is judged through two windows:

- the author's own last 32 messages, anywhere on the server
- the channel's last 16 messages, from anyone

That is how the model knows that one regular always types in capitals while
another never does, and how it tells two friends trading insults apart from a
pile-on.

The labels come from what already happens on the server:

| Source | What it teaches | Weight |
|---|---|---|
| Automod removed it and the case stood | positive, in the categories the rules named | 1 |
| Automod removed it and staff took the case back with `/unwarn` | strong negative: the rules were wrong | 2 |
| A moderator warned, muted, kicked, banned or quarantined the member within ten minutes | `conduct` positive for their last few messages | 0.4–0.7 |
| ✅ or ❌ pressed on a case card or a review card | overrides everything | 3 |
| A day passed with nothing happening to the author, before or after | clean negative | 1 |

`conduct` is the category no rule can see, and it is the reason the model
exists.

The model also predicts the shape of each member's next message. That task
needs no labels, so every message trains it, and the size of the miss on a new
message is its **anomaly** score: how out of character the message is for that
person.

The safeguards:

- **Split by time.** The newest tenth of labeled messages is held out, so a
  model is always judged on a future it never saw.
- **Calibrated.** A Platt fit on the held-out period turns raw scores into
  probabilities. 0.9 means staff acted on nine in ten such messages.
- **Champion vs challenger.** A new model goes live only if it matches or
  beats the live one on the same held-out messages. Every version is kept, and
  `rollback` is instant.
- **Replay.** The quarter-hourly update trains on what is new plus a sample of
  older examples, so one busy afternoon cannot make the model forget the year.
- **It never acts alone.** Hugh puts confident scores in front of staff as
  review cards (`[mind] mode = "review"`). It does not delete, mute or ban on
  the model's word.

## Schedule

| Job | When | What |
|---|---|---|
| Quick update | every 15 minutes, once 200 new messages arrive | embed new messages, fine-tune the live model with replay |
| Nightly | 04:00 local | adapt the encoder to the server's language (masked-LM on the archive), re-embed, train from scratch, clean up |

## Setting up the home server (GTX 10-series)

GTX 10-series cards are Pascal cards (compute capability 6.1). Pascal has no tensor
cores and runs half precision slowly, so Hugh Mind trains it in fp32
automatically. Newer PyTorch CUDA builds have dropped Pascal kernels
altogether, so install from the CUDA 12.6 index:

```sh
python -m venv .venv
. .venv/bin/activate            # Windows: .venv\Scripts\activate
pip install torch --index-url https://download.pytorch.org/whl/cu126
pip install -e ".[test]"
cp mind.example.toml mind.toml
python -m hugh_mind check
```

`check` prints the card, the profile and the precision it chose, and stops
with install instructions if the PyTorch build cannot run on the card.

| Card | Profile | What changes |
|---|---|---|
| GTX 1060 6GB | `6gb` | batch 64; the scorer shares the GPU |
| GTX 1060 3GB | `3gb` | batch 16 with 4× gradient accumulation; the encoder's word table is frozen during pretraining and checkpointing is on; the scorer runs on the CPU so training keeps the card |
| GTX 1050 2GB | `2gb` | as `3gb`, with pretraining batches of 4 messages cut to 64 tokens |
| no GPU | `cpu` | everything on the CPU, slowly |

Then, in two terminals or as two services:

```sh
python -m hugh_mind serve       # the scorer Hugh calls
python -m hugh_mind daemon      # the training schedule
```

and in Hugh's `config.toml`:

```toml
[archive]
enabled = true

[mind]
enabled = true
endpoint = "http://127.0.0.1:8765"
mode = "shadow"     # score silently first; switch to "review" once it earns it
```

With Docker, `docker compose --profile mind up -d` from the repository root
runs both, as described in `docker-compose.yml`.

## Commands

```text
python -m hugh_mind check                 device, profile and archive size
python -m hugh_mind train                 train from scratch now
python -m hugh_mind train --incremental   a quick update now
python -m hugh_mind pretrain              adapt the encoder now
python -m hugh_mind embed                 embed messages the live encoder has not seen
python -m hugh_mind status                the live model and how it scored
python -m hugh_mind rollback              back to the model live before this one
```

## Rolling it out

1. Turn on `[archive]` and let history build. Nothing trains until both the
   training and held-out periods hold at least five messages that needed
   action.
2. Run the daemon and the scorer with `[mind] mode = "shadow"`. Every message
   is scored and the score kept in the archive's `scores` table; staff see
   nothing yet. Case cards gain ✅ and ❌ straight away, so press them.
3. Read `python -m hugh_mind status`. When `actionable.average_precision` is
   well above `actionable.prevalence`, and the threshold it reports sits near
   your `review_threshold`, switch to `mode = "review"`.

## Tests

```sh
pip install torch --index-url https://download.pytorch.org/whl/cpu
pip install -e ".[test]"
pytest
```

The tests build a small synthetic server with a tiny local encoder, so they
download nothing and run on any CPU in under a minute.
