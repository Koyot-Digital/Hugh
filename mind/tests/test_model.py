import numpy as np
import torch

from hugh_mind.archive import Messages
from hugh_mind.config import ModelConfig
from hugh_mind.data import Corpus, assemble
from hugh_mind.labels import LABELS
from hugh_mind.model import HughMind, structure_mask
from hugh_mind.train import average_precision, calibrated, platt, threshold_for

CONFIG = ModelConfig(history=4, channel=3, d_model=16, layers=2, heads=2, author_dim=4, dropout=0.0)


def corpus(count=12, authors=(1, 2), dimension=8):
    rng = np.random.default_rng(0)
    messages = Messages(
        ids=np.arange(100, 100 + count, dtype=np.int64),
        channel=np.array([5 + index % 2 for index in range(count)], dtype=np.int64),
        author=np.array([authors[index % len(authors)] for index in range(count)], dtype=np.int64),
        reply_to=np.zeros(count, dtype=np.int64),
        created_at=np.arange(count, dtype=np.int64) * 60_000,
        features=rng.integers(0, 255, (count, 22), dtype=np.uint8),
        flagged=np.zeros(count, dtype=bool),
        case_id=np.zeros(count, dtype=np.int64),
        deleted=np.zeros(count, dtype=bool),
    )
    text = rng.normal(size=(count, dimension)).astype(np.float16)
    return Corpus.from_messages(messages, text, np.ones(count, dtype=bool))


def batch_for(data, targets):
    targets = np.array(targets)
    hist, chan = data.context(targets, CONFIG.history, CONFIG.channel)
    return assemble(data, targets, hist, chan, np.ones(len(targets), dtype=np.int64))


def test_windows_hold_only_earlier_messages_oldest_first():
    data = corpus()
    hist, chan = data.context(np.array([0, 9]), CONFIG.history, CONFIG.channel)
    assert (hist[0] == -1).all(), "the first message has no history"
    # Message 9 is author 2's (odd rows); its history is the four before it.
    assert hist[1].tolist() == [1, 3, 5, 7]
    # Channel 6 holds the odd rows too, so the windows agree here.
    assert chan[1].tolist() == [3, 5, 7]


def test_every_row_has_something_to_attend_to():
    blocked = structure_mask(CONFIG.history, CONFIG.channel)
    assert not blocked.all(dim=1).any()
    assert not blocked[-1].any(), "the target sees everything"
    # History never sees the target, and never sees the future.
    assert blocked[1 : 1 + CONFIG.history, -1].all()
    assert blocked[1, 2]


def test_padded_windows_do_not_produce_nan():
    model = HughMind(CONFIG, text_dim=8, authors=2).eval()
    output = model(batch_for(corpus(), [0, 1, 11]), need_weights=True)
    assert torch.isfinite(output.logits).all()
    assert torch.isfinite(output.next_features).all()
    assert output.logits.shape == (3, len(LABELS))
    assert torch.allclose(output.attention.sum(-1), torch.ones(3), atol=1e-5)


def test_the_unusual_score_never_sees_the_message_it_judges():
    model = HughMind(CONFIG, text_dim=8, authors=2).eval()
    data = corpus()
    before = model(batch_for(data, [10])).next_features
    data.features[10] = 255 - data.features[10]
    data.text[10] = -data.text[10]
    after = model(batch_for(data, [10]))
    assert torch.allclose(before, after.next_features, atol=1e-6)


def test_new_members_start_at_the_average_member():
    model = HughMind(CONFIG, text_dim=8, authors=2)
    average = model.authors.weight.data[1:].mean(0).clone()
    model.grow_authors(4)
    assert model.authors.num_embeddings == 5
    assert torch.allclose(model.authors.weight.data[3], average)


def test_average_precision_rewards_ranking_positives_first():
    target = np.array([1, 0, 1, 0])
    assert average_precision(np.array([0.9, 0.8, 0.7, 0.1]), target) == (1 + 2 / 3) / 2
    assert average_precision(np.array([0.9, 0.1, 0.8, 0.0]), target) == 1.0
    assert np.isnan(average_precision(np.array([0.5]), np.array([0])))


def test_calibration_turns_overconfident_scores_into_honest_ones():
    rng = np.random.default_rng(1)
    truth = rng.random(4000) < 0.2
    # Scores that are right but four times too confident.
    logits = (np.where(truth, 1.0, -1.0) + rng.normal(0, 1.0, 4000)) * 4
    a, b = platt(logits, truth.astype(float))
    probabilities = calibrated(logits[:, None], [(a, b)])[:, 0]
    high = probabilities > 0.8
    assert abs(truth[high].mean() - probabilities[high].mean()) < 0.05


def test_the_threshold_keeps_precision_at_the_target():
    probabilities = np.array([0.95, 0.9, 0.8, 0.7, 0.6, 0.5])
    target = np.array([1, 1, 1, 0, 1, 0])
    assert threshold_for(probabilities, target, 0.9) == 0.8
    assert threshold_for(probabilities, np.zeros(6), 0.9) is None
