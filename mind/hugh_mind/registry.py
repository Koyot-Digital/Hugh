"""Every model Hugh Mind has trained, and which one is live.

    registry/
      current.json          {"model": "...", "encoder": "..."}
      history.json          every promotion, newest last, for rollback
      state.json            bookkeeping for the scheduler
      versions/<version>/   model.pt and metrics.json
      encoders/<version>/   a pretrained text encoder, in Hugging Face layout

`current.json` is replaced atomically, so the scorer never reads half a pointer.
"""

from __future__ import annotations

import json
import os
import shutil
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import torch

BASE_ENCODER = "base"


class Registry:
    def __init__(self, root: Path, base_encoder: str):
        self.root = root
        self.base_encoder = base_encoder
        self.versions = root / "versions"
        self.encoders = root / "encoders"
        self.versions.mkdir(parents=True, exist_ok=True)
        self.encoders.mkdir(parents=True, exist_ok=True)

    # Pointers -----------------------------------------------------------

    def current(self) -> dict[str, Any]:
        return _read(self.root / "current.json", {})

    def current_model(self) -> str | None:
        return self.current().get("model")

    def current_encoder(self) -> str:
        return self.current().get("encoder", BASE_ENCODER)

    def promote(self, *, model: str | None = None, encoder: str | None = None) -> None:
        pointer = self.current()
        if model is not None:
            pointer["model"] = model
        if encoder is not None:
            pointer["encoder"] = encoder
        _write(self.root / "current.json", pointer)
        history = _read(self.root / "history.json", [])
        history.append({**pointer, "at": _now()})
        _write(self.root / "history.json", history)

    def rollback(self) -> str | None:
        """Steps back to the model live before the current one."""
        current = self.current_model()
        history = _read(self.root / "history.json", [])
        earlier = [
            entry["model"]
            for entry in history
            if entry.get("model") and entry["model"] != current
            and (self.versions / entry["model"] / "model.pt").exists()
        ]
        if not earlier:
            return None
        self.promote(model=earlier[-1])
        return earlier[-1]

    # Models -------------------------------------------------------------

    def new_version(self) -> str:
        stamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
        version, suffix = stamp, 1
        while (self.versions / version).exists() or (self.encoders / version).exists():
            suffix += 1
            version = f"{stamp}-{suffix}"
        return version

    def save_model(self, version: str, checkpoint: dict[str, Any]) -> Path:
        folder = self.versions / version
        folder.mkdir(parents=True, exist_ok=False)
        torch.save(checkpoint, folder / "model.pt")
        _write(folder / "metrics.json", checkpoint.get("metrics", {}))
        return folder

    def load_model(self, version: str, device: torch.device | str = "cpu") -> dict[str, Any]:
        return torch.load(
            self.versions / version / "model.pt", map_location=device, weights_only=False
        )

    def metrics(self, version: str) -> dict[str, Any]:
        return _read(self.versions / version / "metrics.json", {})

    # Encoders -----------------------------------------------------------

    def encoder_source(self, version: str) -> str:
        """Where to load an encoder from: the hub ID or local path in the
        configuration for the base encoder, or the registry for an adapted one."""
        if version == BASE_ENCODER:
            return self.base_encoder
        return str(self.encoders / version)

    def encoder_folder(self, version: str) -> Path:
        return self.encoders / version

    # Bookkeeping --------------------------------------------------------

    def state(self) -> dict[str, Any]:
        return _read(self.root / "state.json", {})

    def update_state(self, **values: Any) -> None:
        state = self.state()
        state.update(values)
        _write(self.root / "state.json", state)

    def prune(self, keep: int) -> list[str]:
        """Deletes all but the newest `keep` models, and every encoder no kept
        model uses, never touching what is live."""
        live_model = self.current_model()
        live_encoder = self.current_encoder()
        models = sorted(path.name for path in self.versions.iterdir() if path.is_dir())
        kept = set(models[-keep:]) | ({live_model} if live_model else set())
        removed = []
        for name in models:
            if name not in kept:
                shutil.rmtree(self.versions / name, ignore_errors=True)
                removed.append(name)
        used = {live_encoder} | {
            self.metrics(name).get("encoder", BASE_ENCODER) for name in kept
        }
        for path in self.encoders.iterdir():
            if path.is_dir() and path.name not in used:
                shutil.rmtree(path, ignore_errors=True)
                removed.append(f"encoder {path.name}")
        return removed


def _read(path: Path, default: Any) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return default


def _write(path: Path, value: Any) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2), encoding="utf-8")
    os.replace(temporary, path)


def _now() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds")
