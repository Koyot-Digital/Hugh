# syntax=docker/dockerfile:1
# Hugh Mind for an NVIDIA card. The default index is CUDA 12.6, the newest
# line that still ships kernels for GTX 10-series (Pascal, sm_61) cards;
# `python -m hugh_mind check` refuses a build without them. A newer card can
# build with --build-arg TORCH_INDEX=https://download.pytorch.org/whl/cu128.
FROM python:3.12-slim

ARG TORCH_INDEX=https://download.pytorch.org/whl/cu126
WORKDIR /app
RUN pip install --no-cache-dir torch --index-url "${TORCH_INDEX}"
COPY pyproject.toml ./
COPY hugh_mind ./hugh_mind
RUN pip install --no-cache-dir . \
    && useradd --system --uid 10001 hugh

USER hugh
# The encoder download is cached beside the models, so a rebuilt container
# does not fetch it again.
ENV HF_HOME=/app/data/mind/hf-cache \
    PYTHONUNBUFFERED=1
ENTRYPOINT ["python", "-m", "hugh_mind", "--config", "/app/mind.toml"]
CMD ["daemon"]
