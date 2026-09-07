use std::collections::{HashSet, VecDeque};

/// Keep raw text (including controls) separate from the gated, visible reply.
/// Assistant events confirm individual blocks; result repeats the final text,
/// not all the commentary and tool work from the turn.
#[derive(Clone, Default)]
pub(super) struct ClaudeTextState {
    blocks: Vec<TextBlock>,
    pending: VecDeque<usize>,
    current: Option<usize>,
    message_id: Option<String>,
    seen_assistant_events: HashSet<String>,
    last_assistant: Option<(Option<String>, String)>,
}

#[derive(Clone)]
struct TextBlock {
    message_id: Option<String>,
    text: String,
}

impl ClaudeTextState {
    pub(super) fn start_message(&mut self, id: Option<&str>) {
        self.message_id = id.map(str::to_owned);
        self.current = None;
    }

    pub(super) fn start_block(&mut self) {
        let index = self.blocks.len();
        self.blocks.push(TextBlock {
            message_id: self.message_id.clone(),
            text: String::new(),
        });
        self.pending.push_back(index);
        self.current = Some(index);
    }

    pub(super) fn push_delta(&mut self, delta: &str) {
        if self.current.is_none() {
            self.start_block();
        }
        self.blocks[self.current.unwrap()].text.push_str(delta);
    }

    pub(super) fn assistant(
        &mut self,
        event_id: Option<&str>,
        message_id: Option<&str>,
        expected: &[String],
    ) -> bool {
        if let Some(id) = event_id {
            if !self.seen_assistant_events.insert(id.to_owned()) {
                return false;
            }
        }
        let identity = (message_id.map(str::to_owned), expected.join(""));
        if event_id.is_none()
            && self.pending.is_empty()
            && self.last_assistant.as_ref() == Some(&identity)
        {
            return false;
        }
        let mut changed = false;
        for text in expected {
            // Match by message and block order, never by a common text prefix:
            // dropped middle deltas and identical paragraphs are both valid.
            let pending = self.pending.iter().position(|index| {
                let id = self.blocks[*index].message_id.as_deref();
                id.is_none() || message_id.is_none() || id == message_id
            });
            if let Some(position) = pending {
                let index = self.pending.remove(position).unwrap();
                changed |= self.blocks[index].text != *text;
                self.blocks[index].text.clone_from(text);
                if self.current == Some(index) {
                    self.current = None;
                }
            } else {
                self.blocks.push(TextBlock {
                    message_id: message_id.map(str::to_owned),
                    text: text.clone(),
                });
                changed = true;
            }
        }
        self.last_assistant = Some(identity);
        changed
    }

    pub(super) fn result(&mut self, expected: &str) -> bool {
        if self
            .pending
            .back()
            .is_some_and(|index| *index + 1 == self.blocks.len())
        {
            let index = self.pending.pop_back().unwrap();
            let changed = self.blocks[index].text != expected;
            self.blocks[index].text = expected.to_owned();
            self.current = None;
            return changed;
        }
        if self
            .last_assistant
            .as_ref()
            .is_some_and(|(_, text)| text == expected)
            || self
                .blocks
                .last()
                .is_some_and(|block| block.text == expected)
        {
            return false;
        }
        self.blocks.push(TextBlock {
            message_id: None,
            text: expected.to_owned(),
        });
        true
    }

    pub(super) fn blocks(&self) -> Vec<String> {
        self.blocks.iter().map(|block| block.text.clone()).collect()
    }
}
