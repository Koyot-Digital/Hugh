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


def test_a_build_runs_on_cards_its_machine_code_covers():
    from hugh_mind.device import can_run

    # torch 2.14.1+cu126, as installed on the home server: no sm_61 listed,
    # yet its sm_60 code runs the GTX 1050.
    server_build = ["sm_50", "sm_60", "sm_70", "sm_75", "sm_80", "sm_86", "sm_90"]
    assert can_run(server_build, 6, 1)
    assert can_run(server_build, 8, 9), "Ada runs on sm_86 code"
    # Newer CUDA builds that dropped Pascal altogether.
    assert not can_run(["sm_75", "sm_80", "sm_86", "sm_90", "sm_100", "sm_120"], 6, 1)
    # Code for a newer minor version never runs on an older card.
    assert not can_run(["sm_62"], 6, 1)
    # PTX for an older capability is compiled by the driver on first use.
    assert can_run(["sm_90", "compute_50"], 6, 1)
    assert not can_run(["compute_70"], 6, 1)


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
