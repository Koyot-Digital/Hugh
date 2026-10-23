"""python -m hugh_mind [--config mind.toml] <command>

    check      show the device, profile and what the archive holds
    embed      embed every archived message the live encoder has not seen
    pretrain   adapt the encoder to the server's language now
    train      train a model now (--incremental for a quick update)
    serve      run the live scorer Hugh calls
    daemon     run the training schedule
    status     show the live model and how it scored
    rollback   go back to the model live before this one
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from pathlib import Path

from . import config as configuration
from . import daemon, device
from .registry import Registry


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="hugh_mind", description="Hugh Mind trainer and scorer")
    parser.add_argument("--config", type=Path, default=Path("mind.toml"))
    parser.add_argument("--verbose", action="store_true")
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("check", "embed", "pretrain", "serve", "daemon", "status", "rollback"):
        commands.add_parser(name)
    train_parser = commands.add_parser("train")
    train_parser.add_argument("--incremental", action="store_true")
    arguments = parser.parse_args(argv)

    logging.basicConfig(
        level=logging.DEBUG if arguments.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s · %(message)s",
    )
    config = configuration.load(arguments.config)
    registry = Registry(config.registry_dir, config.encoder.base)

    if arguments.command == "status":
        version = registry.current_model()
        print(json.dumps({
            "model": version,
            "encoder": registry.current_encoder(),
            "metrics": registry.metrics(version) if version else None,
        }, indent=2))
        return 0
    if arguments.command == "rollback":
        version = registry.rollback()
        print(f"now live: {version}" if version else "there is no earlier model to go back to")
        return 0 if version else 1

    runtime = device.select(config.profile)
    if arguments.command == "check":
        from . import archive

        connection = archive.connect(config.archive)
        try:
            count = connection.execute("SELECT COUNT(*) FROM messages").fetchone()[0]
        finally:
            connection.close()
        print(f"device:   {runtime.description}")
        print(f"scoring:  {runtime.serve_device}")
        print(f"archive:  {config.archive} ({count} messages)")
        print(f"live:     {registry.current_model() or 'no model yet'}")
        return 0
    if arguments.command == "embed":
        encoder = registry.current_encoder()
        live = registry.current_model()
        if live:
            encoder = registry.load_model(live)["encoder"] or encoder
        daemon._embed(config, runtime, registry, encoder)
        return 0
    if arguments.command == "pretrain":
        from .pretrain import pretrain

        version = pretrain(config, runtime, registry)
        print(f"new encoder: {version}" if version else "the live encoder stays")
        return 0
    if arguments.command == "train":
        from .train import NotEnoughData, train

        try:
            if not arguments.incremental and config.train_on_content:
                daemon._embed(config, runtime, registry, registry.current_encoder())
            version = train(config, runtime, registry, incremental=arguments.incremental)
        except NotEnoughData as reason:
            print(f"not enough data yet: {reason}")
            return 1
        print(f"promoted {version}" if version else "the live model stays")
        return 0
    if arguments.command == "serve":
        from .serve import serve

        serve(config, runtime, registry)
        return 0
    if arguments.command == "daemon":
        daemon.run(config, runtime, registry)
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
