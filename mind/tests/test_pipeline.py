"""The whole loop on a synthetic server: embed, pretrain, train, serve, update."""

import sqlite3

import numpy as np
import pytest

from hugh_mind import daemon, device
from hugh_mind.registry import Registry
from hugh_mind.serve import NoModel, Scorer
from hugh_mind.train import NotEnoughData, load_workspace, split, train

from .conftest import features


@pytest.fixture
def runtime():
    return device.select("cpu")


def registry_for(config):
    return Registry(config.registry_dir, config.encoder.base)


def request(message_id, author, channel, at, text):
    return {
        "message_id": str(message_id),
        "channel_id": str(channel),
        "author_id": str(author),
        "created_at": at,
        "content": text,
        "features": list(features(text, 60)),
    }


def test_a_first_model_is_trained_judged_and_promoted(server, runtime):
    config = server["config"]
    registry = registry_for(config)
    daemon._embed(config, runtime, registry, registry.current_encoder())

    version = train(config, runtime, registry, incremental=False)
    assert version is not None
    assert registry.current_model() == version
    metrics = registry.metrics(version)
    actionable = metrics["labels"]["actionable"]
    assert actionable["positives"] > 0
    # Spam and rudeness are easy to tell apart from chat on this server.
    assert actionable["average_precision"] > actionable["prevalence"]

    scorer = Scorer(config, runtime, registry)
    assert scorer.reload()
    at = 1_790_000_000_000
    rude = scorer.score(request(99_001, 3001, 1, at, "you are an idiot and your ideas are trash"))
    calm = scorer.score(request(99_002, 1002, 1, at, "hello there how is everyone doing today"))
    assert rude["model"] == version
    assert 0.0 <= calm["actionable"] <= 1.0
    assert set(rude["probabilities"]) >= {"actionable", "conduct", "spam"}
    assert rude["known_member"] and rude["anomaly"] is not None
    assert rude["focus"], "the scorer names the context it leaned on"
    assert rude["actionable"] > calm["actionable"]


def test_a_quick_update_starts_from_the_live_model(server, runtime):
    config = server["config"]
    registry = registry_for(config)
    daemon.nightly(config, runtime, registry)
    first = registry.current_model()
    assert first is not None

    # Another day of the same server, settled, then a quarter-hourly run.
    connection = sqlite3.connect(config.archive)
    newest, = connection.execute("SELECT MAX(created_at) FROM messages").fetchone()
    rng = np.random.default_rng(5)
    for index in range(300):
        at = newest + 60_000 * (index + 1)
        text = "hello there thanks a lot" if index % 3 else "WOW LOL CAPS"
        connection.execute(
            "INSERT INTO messages (id, channel_id, author_id, created_at, content, features, "
            "feature_version) VALUES (?, ?, ?, ?, ?, ?, 1)",
            (500_000 + index, 1, int(rng.choice([1001, 1006])), at, text, features(text, 60)),
        )
    connection.commit()
    connection.close()

    daemon.quick(config, runtime, registry)
    history = registry.metrics(registry.current_model())
    assert registry.current_model() == first or history["mode"] == "incremental"


def test_pretraining_runs_and_keeps_only_an_encoder_that_helps(server, runtime):
    from hugh_mind.pretrain import pretrain

    config = server["config"]
    registry = registry_for(config)
    version = pretrain(config, runtime, registry)
    if version is None:
        assert registry.current_encoder() == "base"
    else:
        assert registry.current_encoder() == version
        assert (registry.encoder_folder(version) / "config.json").exists()


def test_without_enough_history_nothing_is_trained(tmp_path, encoder_folder, runtime):
    from hugh_mind import config as configuration

    from .conftest import build_server

    built = build_server(tmp_path, days=1)
    config = configuration.parse(
        {"data_dir": str(built["data"]), "encoder": {"base": str(encoder_folder)}}, tmp_path
    )
    with pytest.raises(NotEnoughData):
        split(load_workspace(config, registry_for(config), None), 0.1)


def test_the_scorer_says_so_when_there_is_no_model(server, runtime):
    scorer = Scorer(server["config"], runtime, registry_for(server["config"]))
    assert not scorer.reload()
    with pytest.raises(NoModel):
        scorer.score(request(1, 1001, 1, 0, "hi"))


def test_hugh_can_reach_the_scorer_over_http(server, runtime):
    """The exact JSON src/mind.rs sends: IDs as strings, features as bytes."""
    import json
    import threading
    import urllib.request

    from hugh_mind.serve import make_server

    config = server["config"]
    registry = registry_for(config)
    daemon._embed(config, runtime, registry, registry.current_encoder())
    train(config, runtime, registry, incremental=False)
    http, _ = make_server(config, runtime, registry, port=0)
    thread = threading.Thread(target=http.serve_forever, daemon=True)
    thread.start()
    base = f"http://127.0.0.1:{http.server_address[1]}"
    try:
        with urllib.request.urlopen(f"{base}/health") as response:
            assert json.load(response)["ok"] is True
        body = json.dumps({
            "message_id": "1234567890123456789",
            "channel_id": "1",
            "author_id": "3001",
            "created_at": 1_790_000_000_000,
            "reply_to": "10001",
            "content": "your ideas are trash",
            "features": list(features("your ideas are trash", 30)),
        }).encode()
        sent = urllib.request.Request(
            f"{base}/score", data=body, headers={"Content-Type": "application/json"}
        )
        with urllib.request.urlopen(sent) as response:
            score = json.load(response)
        assert 0.0 <= score["actionable"] <= 1.0
        assert all("channel_id" in focus for focus in score["focus"])

        broken = urllib.request.Request(f"{base}/score", data=b'{"message_id": 1}')
        try:
            urllib.request.urlopen(broken)
        except urllib.error.HTTPError as error:
            assert error.code == 400
        else:
            raise AssertionError("a malformed request must be refused")
    finally:
        http.shutdown()


def test_rollback_returns_to_the_previous_model(server, runtime):
    config = server["config"]
    registry = registry_for(config)
    first = train(config, runtime, registry, incremental=False)
    registry.promote(model=first)
    second_folder = registry.new_version()
    registry.save_model(second_folder, registry.load_model(first))
    registry.promote(model=second_folder)
    assert registry.rollback() == first
