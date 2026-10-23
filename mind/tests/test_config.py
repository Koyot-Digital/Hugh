import pytest

from hugh_mind import config as configuration
from hugh_mind.config import PROFILES, profile_for


@pytest.mark.parametrize(
    ("memory", "profile"),
    [
        (5.93, "6gb"),  # GTX 1060 6GB
        (2.94, "3gb"),  # GTX 1060 3GB
        (1.95, "2gb"),  # GTX 1050
        (0.98, "cpu"),
    ],
)
def test_auto_picks_a_profile_the_card_can_hold(memory, profile):
    assert profile_for(memory) == profile


def test_small_cards_leave_the_gpu_to_training():
    for name in ("2gb", "3gb"):
        assert not PROFILES[name].serve_on_gpu
        assert PROFILES[name].freeze_word_embeddings


def test_unknown_keys_stop_startup(tmp_path):
    with pytest.raises(ValueError, match="no key"):
        configuration.parse({"training": {"epoch": 3}}, tmp_path)
    with pytest.raises(ValueError, match="unknown configuration keys"):
        configuration.parse({"gpu": "1050"}, tmp_path)


def test_paths_resolve_from_the_data_directory(tmp_path):
    parsed = configuration.parse({"data_dir": str(tmp_path / "data")}, tmp_path)
    assert parsed.archive == (tmp_path / "data" / "archive.sqlite3").resolve()
    assert parsed.registry_dir == (tmp_path / "data" / "mind" / "registry").resolve()
