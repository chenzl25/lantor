//! Incremental, line-anchored control framing. Nothing pending here is a chat delta.
use serde_json::json;

use super::{
    complete_json_object_end, strip_agent_event_prefix, AGENT_EVENT_PREFIX, SILENT_REPLY_PREFIX,
};

const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct StreamControlGate {
    pending: String,
    line_start: bool,
    fence: Option<(char, usize)>,
    discarded: bool,
    allow_silent: bool,
}

#[derive(Default)]
pub(crate) struct GatedText {
    pub(crate) visible: String,
    pub(crate) events: Vec<String>,
}

impl StreamControlGate {
    pub(crate) fn new(allow_silent: bool) -> Self {
        Self {
            pending: String::new(),
            line_start: true,
            fence: None,
            discarded: false,
            allow_silent,
        }
    }

    pub(crate) fn push(&mut self, delta: &str) -> GatedText {
        self.pending.push_str(delta);
        self.drain(false, false)
    }

    pub(crate) fn finish(&mut self, preserve_incomplete: bool) -> GatedText {
        self.drain(true, preserve_incomplete)
    }

    fn visible(&mut self, count: usize, output: &mut GatedText) {
        output.visible.push_str(&self.pending[..count]);
        self.line_start = self.pending[..count].ends_with('\n');
        self.pending.drain(..count);
    }

    fn drain(&mut self, terminal: bool, preserve_incomplete: bool) -> GatedText {
        let mut output = GatedText::default();
        if self.discarded {
            self.pending.clear();
            return output;
        }
        while !self.pending.is_empty() {
            if !self.line_start {
                let count = self
                    .pending
                    .find('\n')
                    .map_or(self.pending.len(), |i| i + 1);
                self.visible(count, &mut output);
                continue;
            }
            let line = self
                .pending
                .trim_start_matches(|ch: char| ch.is_whitespace() && ch != '\n');
            if line.is_empty() {
                if terminal {
                    self.visible(self.pending.len(), &mut output);
                }
                break;
            }
            if line.starts_with('\n') {
                self.visible(self.pending.find('\n').unwrap() + 1, &mut output);
                continue;
            }
            // Keep fenced examples literal, including control-looking lines.
            if line.starts_with('`') || line.starts_with('~') {
                let ch = line.chars().next().unwrap();
                let count = line.chars().take_while(|value| *value == ch).count();
                if !terminal && count == line.len() && count < 3 {
                    break;
                }
                if count >= 3 {
                    if !terminal && !line.contains('\n') {
                        break;
                    }
                    let rest = &line[count..line.find('\n').unwrap_or(line.len())];
                    match self.fence {
                        None => self.fence = Some((ch, count)),
                        Some((opening, width))
                            if opening == ch && count >= width && rest.trim().is_empty() =>
                        {
                            self.fence = None
                        }
                        _ => {}
                    }
                    let count = self
                        .pending
                        .find('\n')
                        .map_or(self.pending.len(), |i| i + 1);
                    self.visible(count, &mut output);
                    continue;
                }
            }
            if self.fence.is_some() {
                let count = self
                    .pending
                    .find('\n')
                    .map_or(self.pending.len(), |i| i + 1);
                self.visible(count, &mut output);
                continue;
            }
            let mut candidate = line;
            let mut wrapper_pending = false;
            for wrapper in ["[stdout] ", "[stderr] "] {
                if wrapper.starts_with(candidate) && !terminal {
                    wrapper_pending = true;
                    break;
                }
                if let Some(rest) = candidate.strip_prefix(wrapper) {
                    candidate =
                        rest.trim_start_matches(|ch: char| ch.is_whitespace() && ch != '\n');
                    break;
                }
            }
            if wrapper_pending {
                break;
            }
            if !terminal
                && (AGENT_EVENT_PREFIX.starts_with(candidate)
                    || (self.allow_silent && SILENT_REPLY_PREFIX.starts_with(candidate)))
            {
                break;
            }
            if self.allow_silent {
                if let Some(rest) = candidate.strip_prefix(SILENT_REPLY_PREFIX) {
                    if rest.is_empty()
                        || rest.starts_with(':')
                        || rest.starts_with(char::is_whitespace)
                    {
                        if !terminal && !rest.contains('\n') {
                            break;
                        }
                        let end = self.pending.find('\n').unwrap_or(self.pending.len());
                        let reason = rest
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim_start_matches(':')
                            .trim();
                        output
                            .events
                            .push(json!({"type":"silent", "reason":reason}).to_string());
                        self.pending.drain(..end);
                        self.line_start = true;
                        continue;
                    }
                }
            }
            if let Some(payload) = strip_agent_event_prefix(candidate) {
                if let Some(end) = complete_json_object_end(payload) {
                    let consumed = self.pending.len() - payload.len() + end;
                    output.events.push(payload[..end].to_owned());
                    self.pending.drain(..consumed);
                    // Adjacent controls following a control boundary are supported;
                    // a marker following ordinary prose is always literal.
                    self.line_start = true;
                    continue;
                }
                if terminal {
                    if preserve_incomplete {
                        self.visible(self.pending.len(), &mut output);
                    } else {
                        self.pending.clear();
                    }
                }
                break;
            }
            let count = self
                .pending
                .find('\n')
                .map_or(self.pending.len(), |i| i + 1);
            self.visible(count, &mut output);
        }
        if self.pending.len() > MAX_PENDING_BYTES {
            // Bound malformed/never-ending controls without publishing their tail.
            self.pending.clear();
            self.discarded = true;
            output.events.push(json!({"type":"activity", "kind":"error", "title":"Control output too large", "detail":"An unfinished control exceeded 4MiB; the rest of this message was suppressed."}).to_string());
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_utf8_boundary_filters_controls_without_changing_visible_text() {
        let source = "检查中。\n\n  [stdout] LANTOR_EVENT {\"type\":\"activity\",\"title\":\"a \\\" }\",\"detail\":\"中文🦀\"}\n\n最终结果。\n";
        let expected = "检查中。\n\n\n\n最终结果。\n";
        for split in source.char_indices().map(|(i, _)| i).chain([source.len()]) {
            let mut gate = StreamControlGate::new(true);
            let first = gate.push(&source[..split]);
            assert!(!first.visible.contains("LANTOR_EVENT"));
            assert!(!first.visible.contains("detail"));
            let second = gate.push(&source[split..]);
            let last = gate.finish(false);
            assert_eq!(
                format!("{}{}{}", first.visible, second.visible, last.visible),
                expected,
                "split {split}"
            );
            assert_eq!(
                first.events.len() + second.events.len() + last.events.len(),
                1
            );
        }
    }

    #[test]
    fn literal_inline_and_fenced_examples_survive_character_by_character() {
        let source = "说明 `LANTOR_EVENT {` 后面的文字不能丢。\n```json\nLANTOR_EVENT {\"type\":\"activity\"}\n```\n~~~text\nLANTOR_EVENT {\n~~~\nDone";
        let mut gate = StreamControlGate::new(true);
        let mut visible = String::new();
        for ch in source.chars() {
            let output = gate.push(&ch.to_string());
            assert!(output.events.is_empty());
            visible.push_str(&output.visible);
        }
        visible.push_str(&gate.finish(false).visible);
        assert_eq!(visible, source);
    }

    #[test]
    fn ordinary_text_is_immediate_and_terminal_only_drops_control_tails() {
        let mut gate = StreamControlGate::new(true);
        assert_eq!(gate.push("Hello").visible, "Hello");
        assert_eq!(gate.push("\nLANT").visible, "\n");
        assert!(gate.push("OR_EVENT {\"type\":").visible.is_empty());
        assert!(gate.finish(false).visible.is_empty());
        let mut gate = StreamControlGate::new(true);
        assert!(gate.push("LANT").visible.is_empty());
        assert_eq!(gate.finish(false).visible, "LANT");
    }
}
