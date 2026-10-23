//! A message reduced to numbers: the fixed, versioned vector that the live
//! baselines and the trainer both read.
//!
//! Like `detection.rs`, nothing here touches Discord or the disk. The vector
//! describes the shape of a message rather than its words, so it can outlive
//! the text under any retention setting.

use crate::detection::{
    emoji_count, longest_character_run, longest_combining_run, url_hosts, visible_text,
};

/// Bumped whenever a feature is added, removed, reordered or rescaled, so a
/// stored vector is never read against the wrong layout.
pub const VERSION: u8 = 1;

/// Slots in the vector.
pub const COUNT: usize = 22;

/// One slot of the vector. The discriminant is the slot's index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Length,
    Words,
    Letters,
    CapsRatio,
    DigitRatio,
    PunctuationRatio,
    /// Share of letters outside ASCII, which tells scripts and accents apart.
    NonAsciiRatio,
    MeanWordLength,
    LongestRun,
    CombiningRun,
    Emoji,
    Newlines,
    Links,
    Mentions,
    Attachments,
    Questions,
    Exclamations,
    /// The message holds a code block, which explains newlines and symbols
    /// that would otherwise look like padding.
    CodeBlock,
    Reply,
    Edit,
    HourOfDay,
    /// Seconds since the author's previous message.
    Gap,
}

impl Feature {
    pub const ALL: [Self; COUNT] = [
        Self::Length,
        Self::Words,
        Self::Letters,
        Self::CapsRatio,
        Self::DigitRatio,
        Self::PunctuationRatio,
        Self::NonAsciiRatio,
        Self::MeanWordLength,
        Self::LongestRun,
        Self::CombiningRun,
        Self::Emoji,
        Self::Newlines,
        Self::Links,
        Self::Mentions,
        Self::Attachments,
        Self::Questions,
        Self::Exclamations,
        Self::CodeBlock,
        Self::Reply,
        Self::Edit,
        Self::HourOfDay,
        Self::Gap,
    ];

    const fn scale(self) -> Scale {
        match self {
            Self::CapsRatio
            | Self::DigitRatio
            | Self::PunctuationRatio
            | Self::NonAsciiRatio
            | Self::CodeBlock
            | Self::Reply
            | Self::Edit
            | Self::HourOfDay => Scale::Unit,
            Self::Length | Self::Letters => Scale::Log(4_000.0),
            Self::Words => Scale::Log(1_000.0),
            Self::MeanWordLength => Scale::Log(30.0),
            Self::LongestRun | Self::Newlines => Scale::Log(200.0),
            Self::CombiningRun | Self::Emoji => Scale::Log(100.0),
            Self::Links | Self::Mentions | Self::Questions | Self::Exclamations => Scale::Log(50.0),
            Self::Attachments => Scale::Log(10.0),
            Self::Gap => Scale::Log(GAP_CEILING),
        }
    }
}

/// A week. A longer silence, or no earlier message at all, reads as this.
const GAP_CEILING: f32 = 604_800.0;

/// How a raw value is squeezed into one byte for storage.
#[derive(Debug, Clone, Copy)]
enum Scale {
    /// Already a share or a flag between 0 and 1.
    Unit,
    /// A count or a length, compressed logarithmically up to this ceiling, so
    /// the step from 1 to 5 survives as well as the step from 500 to 2,000.
    Log(f32),
}

/// Everything about a message the vector is built from.
#[derive(Debug)]
pub struct MessageShape<'a> {
    pub content: &'a str,
    pub mentions: usize,
    pub attachments: usize,
    pub is_reply: bool,
    pub is_edit: bool,
    /// Hour of the day in UTC, 0 to 23.
    pub hour_utc: u32,
    /// Seconds since the author's previous message, when Hugh saw one.
    pub gap_seconds: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features([f32; COUNT]);

impl Features {
    #[must_use]
    pub fn extract(shape: &MessageShape<'_>) -> Self {
        let text = visible_text(shape.content);
        let mut letters = 0_usize;
        let mut upper = 0_usize;
        let mut non_ascii = 0_usize;
        let mut digits = 0_usize;
        let mut punctuation = 0_usize;
        let mut printed = 0_usize;
        for character in text.chars() {
            if character.is_whitespace() {
                continue;
            }
            printed += 1;
            if character.is_alphabetic() {
                letters += 1;
                upper += usize::from(character.is_uppercase());
                non_ascii += usize::from(!character.is_ascii());
            } else if character.is_numeric() {
                digits += 1;
            } else if character.is_ascii_punctuation() {
                punctuation += 1;
            }
        }
        let words: Vec<&str> = text.split_whitespace().collect();
        let word_characters: usize = words.iter().map(|word| word.chars().count()).sum();

        let mut values = [0.0; COUNT];
        let mut set = |feature: Feature, value: f32| values[feature as usize] = value;
        set(Feature::Length, count(text.chars().count()));
        set(Feature::Words, count(words.len()));
        set(Feature::Letters, count(letters));
        set(Feature::CapsRatio, share(upper, letters));
        set(Feature::DigitRatio, share(digits, printed));
        set(Feature::PunctuationRatio, share(punctuation, printed));
        set(Feature::NonAsciiRatio, share(non_ascii, letters));
        set(
            Feature::MeanWordLength,
            if words.is_empty() {
                0.0
            } else {
                count(word_characters) / count(words.len())
            },
        );
        set(Feature::LongestRun, count(longest_character_run(&text)));
        set(Feature::CombiningRun, count(longest_combining_run(&text)));
        set(Feature::Emoji, count(emoji_count(&text)));
        set(Feature::Newlines, count(text.matches('\n').count()));
        set(Feature::Links, count(url_hosts(&text).len()));
        set(Feature::Mentions, count(shape.mentions));
        set(Feature::Attachments, count(shape.attachments));
        set(Feature::Questions, count(text.matches('?').count()));
        set(Feature::Exclamations, count(text.matches('!').count()));
        set(Feature::CodeBlock, flag(text.contains("```")));
        set(Feature::Reply, flag(shape.is_reply));
        set(Feature::Edit, flag(shape.is_edit));
        set(
            Feature::HourOfDay,
            count(shape.hour_utc.min(23) as usize) / 23.0,
        );
        #[allow(clippy::cast_possible_truncation)]
        set(
            Feature::Gap,
            shape
                .gap_seconds
                .map_or(GAP_CEILING, |gap| (gap.max(0.0) as f32).min(GAP_CEILING)),
        );
        Self(values)
    }

    #[cfg(test)]
    #[must_use]
    pub const fn get(self, feature: Feature) -> f32 {
        self.0[feature as usize]
    }

    /// One byte per slot: the form the archive keeps.
    #[must_use]
    pub fn quantize(&self) -> [u8; COUNT] {
        let mut bytes = [0; COUNT];
        for (index, feature) in Feature::ALL.into_iter().enumerate() {
            let value = self.0[index];
            let unit = match feature.scale() {
                Scale::Unit => value,
                Scale::Log(ceiling) => value.max(0.0).ln_1p() / ceiling.ln_1p(),
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                bytes[index] = (unit.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        bytes
    }
}

#[allow(clippy::cast_precision_loss)]
const fn count(value: usize) -> f32 {
    value as f32
}

fn share(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        0.0
    } else {
        count(part) / count(whole)
    }
}

const fn flag(value: bool) -> f32 {
    if value { 1.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(content: &str) -> MessageShape<'_> {
        MessageShape {
            content,
            mentions: 0,
            attachments: 0,
            is_reply: false,
            is_edit: false,
            hour_utc: 12,
            gap_seconds: Some(30.0),
        }
    }

    /// The inverse of [`Features::quantize`], for checking what survives it.
    fn dequantize(bytes: &[u8; COUNT]) -> [f32; COUNT] {
        let mut values = [0.0; COUNT];
        for (index, feature) in Feature::ALL.into_iter().enumerate() {
            let unit = f32::from(bytes[index]) / 255.0;
            values[index] = match feature.scale() {
                Scale::Unit => unit,
                Scale::Log(ceiling) => (unit * ceiling.ln_1p()).exp_m1(),
            };
        }
        values
    }

    #[test]
    fn every_slot_is_listed_once_at_its_own_index() {
        for (index, feature) in Feature::ALL.into_iter().enumerate() {
            assert_eq!(feature as usize, index);
        }
    }

    #[test]
    fn caps_are_a_share_of_letters_only() {
        let features = Features::extract(&shape("HELLO there 123!!"));
        assert!((features.get(Feature::CapsRatio) - 0.5).abs() < f32::EPSILON);
        assert!((features.get(Feature::Letters) - 10.0).abs() < f32::EPSILON);
        assert!((features.get(Feature::Exclamations) - 2.0).abs() < f32::EPSILON);
        assert!((features.get(Feature::Words) - 3.0).abs() < f32::EPSILON);
    }

    /// Zero-width padding must not make a message look longer or emptier than
    /// it reads.
    #[test]
    fn invisible_padding_is_ignored() {
        let plain = Features::extract(&shape("free nitro"));
        let padded = Features::extract(&shape("f\u{200b}r\u{200b}ee nitro"));
        assert_eq!(plain, padded);
    }

    #[test]
    fn an_empty_message_has_no_ratios_to_divide_by_zero() {
        let features = Features::extract(&shape(""));
        assert!(Feature::ALL.into_iter().all(|feature| {
            let value = features.get(feature);
            value.is_finite() && value >= 0.0
        }));
        assert!(features.get(Feature::CapsRatio).abs() < f32::EPSILON);
    }

    #[test]
    fn a_first_message_reads_as_a_long_silence() {
        let mut first = shape("hi");
        first.gap_seconds = None;
        let bytes = Features::extract(&first).quantize();
        assert_eq!(bytes[Feature::Gap as usize], 255);
    }

    #[test]
    fn quantizing_keeps_values_close_and_clamps_the_ends() {
        let mut busy = shape("LOOK https://a.example https://b.example\n\n```code```");
        busy.mentions = 3;
        busy.attachments = 50;
        let features = Features::extract(&busy);
        let restored = dequantize(&features.quantize());
        for feature in Feature::ALL {
            let original = features.get(feature);
            let back = restored[feature as usize];
            let ceiling = match feature.scale() {
                Scale::Unit => 1.0,
                Scale::Log(ceiling) => ceiling,
            };
            if original >= ceiling {
                assert!((back - ceiling).abs() / ceiling < 0.001, "{feature:?}");
            } else {
                // One byte of a log scale is a ~3% step at the top of the range.
                let tolerance = (original * 0.04).max(ceiling.ln_1p() / 255.0 + 0.01);
                assert!((back - original).abs() <= tolerance, "{feature:?}");
            }
        }
        assert_eq!(features.quantize()[Feature::Attachments as usize], 255);
    }
}
