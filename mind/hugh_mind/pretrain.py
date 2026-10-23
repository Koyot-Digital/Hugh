"""Teaching the text encoder how this server talks.

Masked-language-model training on the archive's own messages: hide 15% of the
tokens and learn to fill them back in. It needs no labels, so every message
counts, and it is how the encoder picks up the server's slang, in-jokes,
spellings and emoji habits. The adapted encoder is kept only if it predicts
held-out messages better than the one it replaces.
"""

from __future__ import annotations

import logging
import math
import random

import torch

from . import archive
from .config import MindConfig
from .device import Runtime, autocast
from .registry import Registry

log = logging.getLogger(__name__)

# Held-out share of the texts, for judging whether adaptation helped.
EVAL_SHARE = 0.05
MIN_TEXTS = 64
# A new encoder must lower held-out loss by at least this share to be kept.
MIN_IMPROVEMENT = 0.005


def mask_tokens(
    input_ids: torch.Tensor,
    attention: torch.Tensor,
    special_ids: list[int],
    mask_id: int,
    vocabulary: int,
    probability: float,
    generator: torch.Generator,
) -> tuple[torch.Tensor, torch.Tensor]:
    """BERT's recipe: of the chosen tokens, 80% become the mask token, 10% a
    random token, and 10% stay as they are."""
    labels = input_ids.clone()
    chance = torch.full(input_ids.shape, probability)
    special = torch.isin(input_ids, torch.tensor(special_ids)) | (attention == 0)
    chance[special] = 0.0
    chosen = torch.bernoulli(chance, generator=generator).bool()
    labels[~chosen] = -100
    masked = input_ids.clone()
    replaced = torch.bernoulli(torch.full(input_ids.shape, 0.8), generator=generator).bool() & chosen
    masked[replaced] = mask_id
    randomised = (
        torch.bernoulli(torch.full(input_ids.shape, 0.5), generator=generator).bool()
        & chosen
        & ~replaced
    )
    masked[randomised] = torch.randint(vocabulary, input_ids.shape, generator=generator)[randomised]
    return masked, labels


def pretrain(config: MindConfig, runtime: Runtime, registry: Registry) -> str | None:
    """Adapts the live encoder to the archive. Returns the new encoder's
    version when it was kept and promoted."""
    from transformers import AutoModelForMaskedLM, AutoTokenizer

    connection = archive.connect(config.archive)
    try:
        texts = archive.recent_texts(connection, config.pretrain.max_texts)
    finally:
        connection.close()
    if len(texts) < MIN_TEXTS:
        log.info("only %d messages to pretrain on; waiting for more", len(texts))
        return None
    rng = random.Random(config.training.seed)
    rng.shuffle(texts)
    held = max(16, int(len(texts) * EVAL_SHARE))
    evaluation, training = texts[:held], texts[held:]

    source = registry.encoder_source(registry.current_encoder())
    tokenizer = AutoTokenizer.from_pretrained(source)
    model = AutoModelForMaskedLM.from_pretrained(source).to(runtime.device)
    profile = runtime.profile
    if profile.freeze_word_embeddings:
        model.get_input_embeddings().weight.requires_grad_(False)
    if profile.gradient_checkpointing and hasattr(model, "gradient_checkpointing_enable"):
        model.gradient_checkpointing_enable()

    special = list(tokenizer.all_special_ids)
    mask_id = tokenizer.mask_token_id
    if mask_id is None:
        log.warning("the encoder's tokenizer has no mask token, so it cannot be pretrained")
        return None
    vocabulary = len(tokenizer)
    generator = torch.Generator().manual_seed(config.training.seed)

    def encode(chunk: list[str]):
        inputs = tokenizer(
            chunk,
            padding=True,
            truncation=True,
            max_length=profile.mlm_max_length,
            return_tensors="pt",
        )
        masked, labels = mask_tokens(
            inputs["input_ids"],
            inputs["attention_mask"],
            special,
            mask_id,
            vocabulary,
            config.pretrain.mask_probability,
            generator,
        )
        return (
            masked.to(runtime.device),
            inputs["attention_mask"].to(runtime.device),
            labels.to(runtime.device),
        )

    @torch.inference_mode()
    def held_out_loss() -> float:
        model.eval()
        total, batches = 0.0, 0
        for start in range(0, len(evaluation), profile.mlm_batch_size):
            ids, attention, labels = encode(evaluation[start : start + profile.mlm_batch_size])
            if (labels != -100).sum() == 0:
                continue
            with autocast(runtime):
                total += float(model(input_ids=ids, attention_mask=attention, labels=labels).loss)
            batches += 1
        return total / max(batches, 1)

    # The same masks for both measurements, so the comparison is fair.
    generator.manual_seed(config.training.seed + 1)
    before = held_out_loss()
    parameters = [parameter for parameter in model.parameters() if parameter.requires_grad]
    optimizer = torch.optim.AdamW(parameters, lr=config.pretrain.learning_rate, weight_decay=0.01)
    scaler = torch.amp.GradScaler(enabled=runtime.amp_dtype == torch.float16)
    model.train()
    steps = 0
    generator.manual_seed(config.training.seed + 2)
    for start in range(0, len(training), profile.mlm_batch_size):
        ids, attention, labels = encode(training[start : start + profile.mlm_batch_size])
        if (labels != -100).sum() == 0:
            continue
        with autocast(runtime):
            loss = model(input_ids=ids, attention_mask=attention, labels=labels).loss
        scaler.scale(loss / profile.accumulation).backward()
        steps += 1
        if steps % profile.accumulation == 0:
            scaler.unscale_(optimizer)
            torch.nn.utils.clip_grad_norm_(parameters, 1.0)
            scaler.step(optimizer)
            scaler.update()
            optimizer.zero_grad(set_to_none=True)
        if steps % 500 == 0:
            log.info("pretraining step %d · loss %.4f", steps, float(loss))
    generator.manual_seed(config.training.seed + 1)
    after = held_out_loss()
    log.info(
        "pretraining on %d messages: held-out loss %.4f → %.4f (perplexity %.1f → %.1f)",
        len(training),
        before,
        after,
        math.exp(min(before, 20)),
        math.exp(min(after, 20)),
    )
    if not after < before * (1.0 - MIN_IMPROVEMENT):
        log.info("the adapted encoder was no better, so the live one stays")
        return None

    version = registry.new_version()
    folder = registry.encoder_folder(version)
    model.save_pretrained(folder)
    tokenizer.save_pretrained(folder)
    registry.promote(encoder=version)
    return version
