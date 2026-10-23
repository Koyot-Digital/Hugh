"""The live scorer Hugh calls for each message.

    POST /score   one message, as Hugh saw it      → probabilities and reasons
    GET  /health  whether a model is loaded, and which

Hugh sends the message itself, because its archive write may still be in the
writer's queue; the context around it comes from the archive. The scorer picks
up a newly promoted model within `serve.reload_seconds`, with no restart.
"""

from __future__ import annotations

import json
import logging
import sqlite3
import threading
import time
from dataclasses import replace
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import torch

from . import archive
from .archive import FEATURE_COUNT, FEATURE_VERSION
from .config import MindConfig
from .data import Corpus, assemble, author_slots
from .device import Runtime, autocast
from .embeddings import EmbeddingStore, Encoder
from .labels import LABELS
from .model import surprise
from .registry import Registry
from .train import calibrated, restore

log = logging.getLogger(__name__)

MAX_BODY = 64 * 1024
FOCUS = 3


class NoModel(Exception):
    pass


class Scorer:
    def __init__(self, config: MindConfig, runtime: Runtime, registry: Registry):
        self.config = config
        self.registry = registry
        device = runtime.serve_device
        self.runtime = replace(
            runtime, device=device, amp_dtype=runtime.amp_dtype if device.type == "cuda" else None
        )
        self.lock = threading.Lock()
        self.version: str | None = None
        self.model = None
        self.vocabulary: dict[int, int] = {}
        self.checkpoint: dict = {}
        self.encoder: Encoder | None = None

    def reload(self) -> bool:
        version = self.registry.current_model()
        if version is None or version == self.version:
            return False
        checkpoint = self.registry.load_model(version, "cpu")
        model, vocabulary = restore(checkpoint, self.runtime.device)
        encoder = None
        if checkpoint["encoder"]:
            encoder = Encoder(
                self.registry.encoder_source(checkpoint["encoder"]),
                checkpoint["encoder"],
                self.runtime.device,
                self.config.encoder.max_length,
            )
        with self.lock:
            self.model, self.vocabulary, self.checkpoint, self.encoder = (
                model,
                vocabulary,
                checkpoint,
                encoder,
            )
            self.version = version
        log.info("scoring with model %s on %s", version, self.runtime.device)
        return True

    def score(self, request: dict) -> dict:
        started = time.perf_counter()
        target = _target(request)
        with self.lock:
            if self.model is None:
                raise NoModel("no model has been trained yet")
            model, checkpoint, encoder = self.model, self.checkpoint, self.encoder
            vocabulary = self.vocabulary
            history_size, channel_size = model.config.history, model.config.channel
            history, channel = self._context(target, history_size, channel_size)
            rows = [*history, *channel, target]
            text, has_text = self._text(rows, encoder, model.text_dim)
            corpus = Corpus(
                ids=np.array([row["id"] for row in rows], dtype=np.int64),
                channel=np.array([row["channel"] for row in rows], dtype=np.int64),
                author=np.array([row["author"] for row in rows], dtype=np.int64),
                reply_to=np.array([row["reply_to"] for row in rows], dtype=np.int64),
                created_at=np.array([row["created_at"] for row in rows], dtype=np.int64),
                features=np.stack([row["features"] for row in rows]),
                text=text,
                has_text=has_text,
            )
            hist = _padded(np.arange(len(history)), history_size)
            chan = _padded(len(history) + np.arange(len(channel)), channel_size)
            slots = author_slots(vocabulary, np.array([target["author"]]))
            batch = assemble(corpus, np.array([len(rows) - 1]), hist[None], chan[None], slots)
            batch = {key: value.to(self.runtime.device) for key, value in batch.items()}
            with torch.inference_mode(), autocast(self.runtime):
                output = model(batch, need_weights=True)
            logits = output.logits.float().cpu().numpy()
            probabilities = calibrated(logits, checkpoint["calibration"])[0]

            anomaly = None
            if bool(batch["next_valid"][0, -1]) and checkpoint["anomaly"]:
                error = float(
                    surprise(output.next_features[:, -1], batch["next_features"][:, -1])[0]
                )
                anomaly = float(np.searchsorted(checkpoint["anomaly"], error) / 100.0)

            focus = []
            if output.attention is not None:
                # Position 0 is the author token and the last is the target.
                weights = output.attention[0, 1:-1].float().cpu().numpy()
                context_rows = batch["rows"][0, :-1].cpu().numpy()
                for position in np.argsort(-weights)[:FOCUS]:
                    row = int(context_rows[position])
                    if row >= 0 and weights[position] > 0:
                        focus.append(
                            {
                                "message_id": str(rows[row]["id"]),
                                "channel_id": str(rows[row]["channel"]),
                                "weight": round(float(weights[position]), 4),
                            }
                        )

        scores = {name: round(float(value), 4) for name, value in zip(LABELS, probabilities, strict=True)}
        categories = {name: value for name, value in scores.items() if name != "actionable"}
        return {
            "model": self.version,
            "actionable": scores["actionable"],
            "probabilities": scores,
            "top": max(categories, key=categories.get),
            "threshold": checkpoint["thresholds"].get("actionable"),
            "anomaly": anomaly,
            "known_member": bool(slots[0]),
            "focus": focus,
            "elapsed_ms": round((time.perf_counter() - started) * 1000, 1),
        }

    def _context(self, target: dict, history: int, channel: int):
        connection = archive.connect(self.config.archive)
        try:
            def before(key: str, value: int, limit: int) -> list[dict]:
                rows = connection.execute(
                    "SELECT id, channel_id, author_id, COALESCE(reply_to, 0), created_at, "
                    f"features, content FROM messages WHERE {key} = ? AND created_at <= ? "
                    "AND id != ? AND feature_version = ? ORDER BY created_at DESC, id DESC LIMIT ?",
                    (value, target["created_at"], target["id"], FEATURE_VERSION, limit),
                ).fetchall()
                return [_row(values) for values in reversed(rows)]

            return (
                before("author_id", target["author"], history),
                before("channel_id", target["channel"], channel),
            )
        except sqlite3.OperationalError as error:
            log.warning("could not read context from the archive: %s", error)
            return [], []
        finally:
            connection.close()

    def _text(self, rows: list[dict], encoder: Encoder | None, dimension: int):
        text = np.zeros((len(rows), dimension), dtype=np.float16)
        has_text = np.zeros(len(rows), dtype=bool)
        if encoder is None:
            return text, has_text
        store = EmbeddingStore(self.config.embeddings_path)
        try:
            cached = store.fetch([row["id"] for row in rows[:-1]], encoder.version)
        finally:
            store.close()
        missing = [
            index
            for index, row in enumerate(rows)
            if row["id"] not in cached and row.get("content")
        ]
        for index, row in enumerate(rows):
            if row["id"] in cached:
                text[index] = cached[row["id"]]
                has_text[index] = True
        if missing:
            vectors = encoder.embed([rows[index]["content"] for index in missing])
            for index, vector in zip(missing, vectors, strict=True):
                text[index] = vector
                has_text[index] = True
        return text, has_text


def _target(request: dict) -> dict:
    try:
        features = [int(value) for value in request["features"]]
        if len(features) != FEATURE_COUNT or not all(0 <= value <= 255 for value in features):
            raise ValueError(f"features must be {FEATURE_COUNT} bytes")
        return {
            "id": int(request["message_id"]),
            "channel": int(request["channel_id"]),
            "author": int(request["author_id"]),
            "reply_to": int(request.get("reply_to") or 0),
            "created_at": int(request["created_at"]),
            "content": str(request.get("content") or ""),
            "features": np.array(features, dtype=np.uint8),
        }
    except (KeyError, TypeError) as error:
        raise ValueError(f"malformed score request: {error}") from error


def _row(values: tuple) -> dict:
    message_id, channel, author, reply_to, created_at, blob, content = values
    return {
        "id": message_id,
        "channel": channel,
        "author": author,
        "reply_to": reply_to,
        "created_at": created_at,
        "features": np.frombuffer(blob, dtype=np.uint8, count=FEATURE_COUNT).copy(),
        "content": content,
    }


def _padded(indices: np.ndarray, size: int) -> np.ndarray:
    indices = indices[-size:]
    return np.concatenate([np.full(size - len(indices), -1, dtype=np.int64), indices.astype(np.int64)])


def serve(config: MindConfig, runtime: Runtime, registry: Registry) -> None:
    server, _ = make_server(config, runtime, registry)
    log.info("Hugh Mind is listening on http://%s:%d", *server.server_address[:2])
    server.serve_forever()


def make_server(
    config: MindConfig, runtime: Runtime, registry: Registry, port: int | None = None
) -> tuple[ThreadingHTTPServer, Scorer]:
    scorer = Scorer(config, runtime, registry)
    scorer.reload()

    def watch() -> None:
        while True:
            time.sleep(config.serve.reload_seconds)
            try:
                scorer.reload()
            except Exception:
                log.exception("failed to load the newly promoted model; the old one stays")

    threading.Thread(target=watch, name="reload", daemon=True).start()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802
            if self.path != "/health":
                self._send(HTTPStatus.NOT_FOUND, {"error": "not found"})
                return
            self._send(HTTPStatus.OK, {"ok": scorer.version is not None, "model": scorer.version})

        def do_POST(self) -> None:  # noqa: N802
            if self.path != "/score":
                self._send(HTTPStatus.NOT_FOUND, {"error": "not found"})
                return
            length = int(self.headers.get("Content-Length") or 0)
            if length <= 0 or length > MAX_BODY:
                self._send(HTTPStatus.BAD_REQUEST, {"error": "body missing or too large"})
                return
            try:
                result = scorer.score(json.loads(self.rfile.read(length)))
            except NoModel as error:
                self._send(HTTPStatus.SERVICE_UNAVAILABLE, {"error": str(error)})
            except (ValueError, json.JSONDecodeError) as error:
                self._send(HTTPStatus.BAD_REQUEST, {"error": str(error)})
            except Exception:
                log.exception("scoring failed")
                self._send(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": "scoring failed"})
            else:
                self._send(HTTPStatus.OK, result)

        def _send(self, status: HTTPStatus, body: dict) -> None:
            payload = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, format: str, *args) -> None:  # noqa: A002
            log.debug("%s %s", self.address_string(), format % args)

    address = (config.serve.host, config.serve.port if port is None else port)
    return ThreadingHTTPServer(address, Handler), scorer
