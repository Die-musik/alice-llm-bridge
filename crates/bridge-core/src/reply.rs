const CONTINUATION_SUFFIX: &str = " Продолжать?";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedReply {
    pub spoken: String,
    pub remaining: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationDecision {
    Continue,
    Stop,
    Empty,
}

#[derive(Debug, Clone, Copy)]
pub struct ReplyShaper {
    limit: usize,
}

impl ReplyShaper {
    pub fn new(limit: usize) -> Self {
        assert!(
            limit > CONTINUATION_SUFFIX.chars().count(),
            "reply limit must leave room for the continuation question"
        );
        Self { limit }
    }

    pub fn split(&self, text: &str) -> ShapedReply {
        let content_limit = self.limit - CONTINUATION_SUFFIX.chars().count();
        let mut chunks = split_into_chunks(text, content_limit).into_iter();
        let first = chunks.next().unwrap_or_default();
        let remaining: Vec<_> = chunks.collect();

        let spoken = if remaining.is_empty() {
            first
        } else {
            first + CONTINUATION_SUFFIX
        };

        ShapedReply { spoken, remaining }
    }
}

impl ContinuationDecision {
    pub fn from_utterance(text: &str) -> Self {
        let normalized = text
            .trim()
            .trim_end_matches(|character: char| {
                character.is_whitespace() || ".!?,;:…".contains(character)
            })
            .to_lowercase();

        if normalized.is_empty() {
            return Self::Empty;
        }

        match normalized.as_str() {
            "нет" | "не надо" | "не продолжай" | "хватит" | "стоп" | "отмена" => {
                Self::Stop
            }
            _ => Self::Continue,
        }
    }
}

fn split_into_chunks(mut text: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();

    while text.chars().count() > limit {
        let exact_end = text
            .char_indices()
            .nth(limit)
            .map(|(index, _)| index)
            .unwrap_or(text.len());
        let prefix = &text[..exact_end];
        let split_at = preferred_split(prefix).unwrap_or(exact_end);

        chunks.push(text[..split_at].to_owned());
        text = &text[split_at..];
    }

    if !text.is_empty() || chunks.is_empty() {
        chunks.push(text.to_owned());
    }

    chunks
}

fn preferred_split(prefix: &str) -> Option<usize> {
    if let Some(index) = prefix.rfind("\n\n") {
        let end = index + 2;
        if end > 0 {
            return Some(end);
        }
    }

    if let Some((index, character)) = prefix
        .char_indices()
        .rev()
        .find(|(_, character)| ".!?…".contains(*character))
    {
        return Some(index + character.len_utf8());
    }

    prefix
        .char_indices()
        .rev()
        .find(|(index, character)| *index > 0 && character.is_whitespace())
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::{ContinuationDecision, ReplyShaper};

    #[test]
    fn long_reply_preserves_text_and_stays_under_limit() {
        let input = format!("{} {}", "А".repeat(700), "Б".repeat(300));
        let shaped = ReplyShaper::new(850).split(&input);

        assert!(shaped.spoken.ends_with(" Продолжать?"));
        assert!(shaped.spoken.chars().count() <= 850);
        let rebuilt = format!(
            "{}{}",
            shaped.spoken.trim_end_matches(" Продолжать?"),
            shaped.remaining.join("")
        );
        assert_eq!(rebuilt, input);
    }

    #[test]
    fn explicit_refusal_stops_but_any_other_word_continues() {
        for value in [
            "нет",
            "Не надо!",
            "не продолжай",
            "хватит",
            "стоп",
            "отмена",
        ] {
            assert_eq!(
                ContinuationDecision::from_utterance(value),
                ContinuationDecision::Stop
            );
        }
        assert_eq!(
            ContinuationDecision::from_utterance("ага"),
            ContinuationDecision::Continue
        );
        assert_eq!(
            ContinuationDecision::from_utterance("включи свет"),
            ContinuationDecision::Continue
        );
        assert_eq!(
            ContinuationDecision::from_utterance("   "),
            ContinuationDecision::Empty
        );
    }
}
