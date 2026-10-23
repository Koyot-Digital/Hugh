"""The schedule: a quick update every quarter hour, a full rebuild every night.

- **Quarter-hourly** (once enough new messages have arrived): embed what is
  new with the live model's encoder, then fine-tune the live model on what has
  settled since it was trained, with replay.
- **Nightly**: adapt the encoder to the server's language if there is enough
  new text, embed everything with the live encoder, train from scratch, then
  delete embeddings and versions nothing needs any more.

Both paths go through the same champion-vs-challenger check, so a bad night
can never replace a good model.
"""

from __future__ import annotations

import logging
import os
import time
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path

from . import archive
from .config import MindConfig
from .device import Runtime
from .embeddings import EmbeddingStore, Encoder, embed_pending
from .pretrain import pretrain
from .registry import BASE_ENCODER, Registry
from .train import NotEnoughData, train

log = logging.getLogger(__name__)

TICK_SECONDS = 30
# A lock older than this belongs to a run that died without cleaning up.
STALE_LOCK_SECONDS = 6 * 3_600


def run(config: MindConfig, runtime: Runtime, registry: Registry) -> None:
    log.info("Hugh Mind trainer started on %s", runtime.description)
    last_quick = 0.0
    while True:
        now = datetime.now()
        try:
            if now.hour == config.schedule.nightly_hour and registry.state().get(
                "nightly_date"
            ) != now.date().isoformat():
                nightly(config, runtime, registry)
                registry.update_state(nightly_date=now.date().isoformat())
                last_quick = time.monotonic()
            elif time.monotonic() - last_quick >= config.schedule.incremental_minutes * 60:
                quick(config, runtime, registry)
                last_quick = time.monotonic()
        except NotEnoughData as reason:
            log.info("not training yet: %s", reason)
            last_quick = time.monotonic()
        except Exception:
            log.exception("a training run failed; the live model is unchanged")
            last_quick = time.monotonic()
        time.sleep(TICK_SECONDS)


def quick(config: MindConfig, runtime: Runtime, registry: Registry) -> str | None:
    newest = _newest_message(config)
    since = registry.state().get("quick_newest", 0)
    if newest - since < config.schedule.min_new_messages and registry.current_model():
        return None
    with _lock(config.mind_dir):
        live = registry.current_model()
        encoder = registry.load_model(live)["encoder"] if live else registry.current_encoder()
        if config.train_on_content and encoder:
            _embed(config, runtime, registry, encoder)
        promoted = train(config, runtime, registry, incremental=True)
        registry.update_state(quick_newest=newest)
        return promoted


def nightly(config: MindConfig, runtime: Runtime, registry: Registry) -> str | None:
    with _lock(config.mind_dir):
        newest = _newest_message(config)
        if config.train_on_content:
            pretrained = registry.state().get("pretrained_newest", 0)
            if newest - pretrained >= config.pretrain.min_new_texts:
                pretrain(config, runtime, registry)
                registry.update_state(pretrained_newest=newest)
            _embed(config, runtime, registry, registry.current_encoder())
        promoted = train(config, runtime, registry, incremental=False)
        registry.update_state(quick_newest=newest)
        live = registry.current_model()
        keep = {registry.current_encoder()}
        if live:
            keep.add(registry.load_model(live)["encoder"] or BASE_ENCODER)
        store = EmbeddingStore(config.embeddings_path)
        try:
            dropped = store.drop_other_encoders(keep)
        finally:
            store.close()
        if dropped:
            log.info("dropped %d embeddings from retired encoders", dropped)
        return promoted


def _embed(config: MindConfig, runtime: Runtime, registry: Registry, encoder_version: str) -> int:
    encoder = Encoder(
        registry.encoder_source(encoder_version),
        encoder_version,
        runtime.device,
        config.encoder.max_length,
    )
    connection = archive.connect(config.archive)
    store = EmbeddingStore(config.embeddings_path)
    try:
        done = embed_pending(connection, store, encoder, runtime.profile.embed_batch_size)
    finally:
        store.close()
        connection.close()
    if done:
        log.info("embedded %d new messages with encoder %s", done, encoder_version)
    return done


def _newest_message(config: MindConfig) -> int:
    """How many messages the archive holds, as a cheap measure of progress."""
    connection = archive.connect(config.archive)
    try:
        return int(connection.execute("SELECT COUNT(*) FROM messages").fetchone()[0])
    finally:
        connection.close()


@contextmanager
def _lock(folder: Path):
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / "train.lock"
    try:
        if time.time() - path.stat().st_mtime > STALE_LOCK_SECONDS:
            path.unlink()
    except FileNotFoundError:
        pass
    try:
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError:
        raise RuntimeError(f"another training run holds {path}") from None
    try:
        os.write(descriptor, str(os.getpid()).encode())
        os.close(descriptor)
        yield
    finally:
        path.unlink(missing_ok=True)
