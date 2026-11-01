use std::collections::{HashSet, VecDeque};

use crate::events::control::StreamControlGate;

/// Prose written just before a tool call that is shorter than this (visible
/// characters, controls excluded) is a progress note, not part of the reply.
pub(super) const INTERIM_TEXT_MAX_CHARS: usize = 400;

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
    /// Number of blocks that precede the latest main-conversation tool call.
    blocks_before_tool: usize,
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

    #[cfg(test)]
    pub(super) fn blocks(&self) -> Vec<String> {
        self.blocks.iter().map(|block| block.text.clone()).collect()
    }

    /// A main-conversation tool call started: every block so far preceded a
    /// tool call. Returns the visible prose of blocks that just became interim
    /// progress notes, so the caller can record them as activity.
    pub(super) fn mark_tool_use(&mut self) -> Vec<String> {
        let start = self.blocks_before_tool;
        self.blocks_before_tool = self.blocks_before_tool.max(self.blocks.len());
        self.blocks[start..self.blocks_before_tool]
            .iter()
            .map(|block| visible_prose(&block.text))
            .filter(|prose| is_interim_prose(prose))
            .collect()
    }

    /// Blocks that make up the reply. Short prose written before a tool call
    /// ("Now checking X.") is a progress note: it is shown while the turn runs,
    /// then reduced to its control lines so it does not open the final reply.
    /// The last block with visible prose is always kept, so a turn whose only
    /// prose preceded its last tool call still replies.
    pub(super) fn visible_blocks(&self) -> Vec<String> {
        self.blocks
            .iter()
            .zip(self.interim_flags())
            .map(|(block, interim)| {
                if interim {
                    control_lines_only(&block.text)
                } else {
                    block.text.clone()
                }
            })
            .collect()
    }

    /// Whether `visible_blocks` drops any prose from the raw blocks.
    pub(super) fn has_interim_prose(&self) -> bool {
        self.interim_flags().into_iter().any(|interim| interim)
    }

    fn interim_flags(&self) -> Vec<bool> {
        let prose: Vec<String> = self
            .blocks
            .iter()
            .map(|block| visible_prose(&block.text))
            .collect();
        let keep = prose.iter().rposition(|text| !text.is_empty());
        prose
            .iter()
            .enumerate()
            .map(|(index, text)| {
                index < self.blocks_before_tool && Some(index) != keep && is_interim_prose(text)
            })
            .collect()
    }
}

fn gate_block(text: &str) -> (String, Vec<String>) {
    let mut gate = StreamControlGate::new(true);
    let mut output = gate.push(text);
    let tail = gate.finish(false);
    output.visible.push_str(&tail.visible);
    output.events.extend(tail.events);
    (output.visible, output.events)
}

/// Prose a block shows in chat, with control lines removed and trimmed.
fn visible_prose(text: &str) -> String {
    gate_block(text).0.trim().to_owned()
}

fn is_interim_prose(prose: &str) -> bool {
    !prose.is_empty() && prose.chars().count() < INTERIM_TEXT_MAX_CHARS
}

/// Keep a dropped block's controls so a reconcile replays them (receipts make
/// the replay idempotent); only its prose leaves the reply.
fn control_lines_only(text: &str) -> String {
    gate_block(text)
        .1
        .into_iter()
        .map(|json| format!("LANTOR_EVENT {json}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(blocks: &[&str], tool_after: &[usize]) -> ClaudeTextState {
        let mut state = ClaudeTextState::default();
        for (index, text) in blocks.iter().enumerate() {
            state.start_block();
            state.push_delta(text);
            if tool_after.contains(&index) {
                state.mark_tool_use();
            }
        }
        state
    }

    #[test]
    fn without_tool_calls_every_block_is_kept() {
        let state = state_with(&["Reading.", "Answer."], &[]);
        assert!(!state.has_interim_prose());
        assert_eq!(state.visible_blocks(), state.blocks());
    }

    #[test]
    fn short_prose_before_a_tool_call_keeps_only_its_controls() {
        let state = state_with(
            &[
                "Checking.\nLANTOR_EVENT {\"type\":\"activity\",\"title\":\"x\"}",
                "Answer.",
            ],
            &[0],
        );
        assert!(state.has_interim_prose());
        assert_eq!(
            state.visible_blocks(),
            vec![
                "LANTOR_EVENT {\"type\":\"activity\",\"title\":\"x\"}\n".to_owned(),
                "Answer.".to_owned()
            ]
        );
    }

    #[test]
    fn mark_tool_use_reports_each_note_once() {
        let mut state = ClaudeTextState::default();
        state.start_block();
        state.push_delta("First note.");
        assert_eq!(state.mark_tool_use(), vec!["First note.".to_owned()]);
        assert!(state.mark_tool_use().is_empty());
        state.start_block();
        state.push_delta("LANTOR_EVENT {\"type\":\"activity\",\"title\":\"x\"}");
        assert!(
            state.mark_tool_use().is_empty(),
            "control-only blocks are not notes"
        );
    }

    #[test]
    fn fenced_control_examples_count_as_prose() {
        let block = "```\nLANTOR_EVENT {\"type\":\"activity\"}\n```";
        let state = state_with(&[block, "Answer."], &[0]);
        assert_eq!(
            state.visible_blocks(),
            vec![String::new(), "Answer.".to_owned()]
        );
    }

    #[test]
    fn silent_marker_is_kept_as_the_same_control() {
        let kept = control_lines_only("Note.\nLANTOR_SILENT_REPLY: nothing to add");
        let mut gate = StreamControlGate::new(true);
        let mut events = gate.push(&kept).events;
        events.extend(gate.finish(false).events);
        assert_eq!(events, gate_block("LANTOR_SILENT_REPLY: nothing to add").1);
    }
}
