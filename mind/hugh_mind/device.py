"""Choosing where Hugh Mind runs, and refusing a PyTorch that cannot use the card.

GTX 10-series cards, such as the home server's GTX 1050, are Pascal cards
(compute capability 6.1). Pascal has
no tensor cores and runs half precision slowly, so it trains in fp32. Newer
PyTorch CUDA builds no longer ship Pascal kernels at all; on such a build the
card is visible but every kernel launch fails, so that is checked up front.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass

import torch

from .config import PROFILES, Profile, profile_for

log = logging.getLogger(__name__)

PASCAL_INSTALL = (
    "pip install torch --index-url https://download.pytorch.org/whl/cu126 "
    "(CUDA 12.6 builds still carry kernels for GTX 10-series cards)"
)


@dataclass(frozen=True)
class Runtime:
    device: torch.device
    profile: Profile
    # None trains in fp32. Pascal always lands here.
    amp_dtype: torch.dtype | None
    description: str

    @property
    def serve_device(self) -> torch.device:
        if self.device.type == "cuda" and self.profile.serve_on_gpu:
            return self.device
        return torch.device("cpu")


def select(profile_name: str = "auto") -> Runtime:
    if not torch.cuda.is_available():
        profile = PROFILES["cpu" if profile_name == "auto" else profile_name]
        return Runtime(torch.device("cpu"), profile, None, "CPU (no usable CUDA device)")

    index = torch.cuda.current_device()
    name = torch.cuda.get_device_name(index)
    major, minor = torch.cuda.get_device_capability(index)
    _require_kernels(name, major, minor)
    total_gb = torch.cuda.get_device_properties(index).total_memory / 1024**3
    if profile_name == "auto":
        profile_name = profile_for(total_gb)
    profile = PROFILES[profile_name]
    if profile.name == "cpu":
        return Runtime(torch.device("cpu"), profile, None, f"CPU ({name} is too small)")

    if major >= 8:
        amp = torch.bfloat16
    elif major >= 7:
        amp = torch.float16
    else:
        amp = None
    precision = "fp32" if amp is None else str(amp).removeprefix("torch.")
    description = (
        f"{name} (sm_{major}{minor}, {total_gb:.1f} GB) · profile {profile.name} · {precision}"
    )
    return Runtime(torch.device("cuda", index), profile, amp, description)


def can_run(built: list[str], major: int, minor: int) -> bool:
    """Whether a build with these architectures runs on a card of this
    compute capability.

    CUDA guarantees that machine code built for one capability runs on any
    card of the same major version and an equal or higher minor version, so
    the sm_60 code in today's builds runs a GTX 1050 (sm_61) even though no
    build lists sm_61. PTX for any capability up to the card's own also runs,
    compiled by the driver on first use."""
    for item in built:
        prefix, _, digits = item.partition("_")
        if not digits.isdigit():
            continue
        built_major, built_minor = divmod(int(digits), 10)
        if prefix == "sm" and built_major == major and built_minor <= minor:
            return True
        if prefix == "compute" and (built_major, built_minor) <= (major, minor):
            return True
    return False


def _require_kernels(name: str, major: int, minor: int) -> None:
    built = torch.cuda.get_arch_list()
    if can_run(built, major, minor):
        return
    arch = f"sm_{major}{minor}"
    hint = PASCAL_INSTALL if major == 6 else "install a PyTorch build that supports it"
    raise RuntimeError(
        f"this PyTorch build ({torch.__version__}, kernels for {', '.join(built)}) cannot run "
        f"on the {name} ({arch}); {hint}"
    )


def autocast(runtime: Runtime):
    """Mixed precision where the card is good at it, plain fp32 elsewhere."""
    if runtime.amp_dtype is None:
        return torch.autocast(device_type=runtime.device.type, enabled=False)
    return torch.autocast(device_type=runtime.device.type, dtype=runtime.amp_dtype)
