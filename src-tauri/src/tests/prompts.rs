use super::{
    build_codex_streaming_prompt, build_streaming_work_item_prompt, build_work_item_prompt,
    claude_system_prompt, ensure_agent_workspace, load_agent_memory_context,
    AGENT_MEMORY_CONTEXT_LIMIT, WORK_ITEM_FINISH_PROMPT,
};
use uuid::Uuid;

#[test]
fn memory_context_is_bounded_and_preserves_tail() {
    let dir = std::env::temp_dir().join(format!("lantor-memory-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp memory dir");
    let memory_path = dir.join("MEMORY.md");
    let memory = format!(
        "# Agent\n\n{}\n\n## Active Context\nimportant tail survives",
        "context ".repeat(AGENT_MEMORY_CONTEXT_LIMIT)
    );
    std::fs::write(&memory_path, memory).expect("write memory");

    let context =
        load_agent_memory_context(dir.to_str().expect("utf8 temp dir")).expect("load memory");
    std::fs::remove_dir_all(&dir).ok();

    let context = context.expect("memory should load");
    assert!(context.contains("Lantor omitted"));
    assert!(context.contains("important tail survives"));
    assert!(context.chars().count() < AGENT_MEMORY_CONTEXT_LIMIT + 1_000);
}

#[test]
fn runtime_standing_prompt_carries_memory_once() {
    let prompt = claude_system_prompt("tester", Some("Persistent memory: prefer concise replies"));
    assert!(prompt.contains("one warm runtime session per agent"));
    assert!(prompt.contains("channel and thread are delivered as message envelope fields"));
    assert!(prompt.contains("Treat messages as conversation"));
    assert!(prompt.contains("Activity events are the short progress notes"));
    assert!(prompt.contains("MEMORY.md is the compact index"));
    assert!(prompt.contains("raw conversation/tool logs should stay out of memory"));
    assert!(prompt.contains("notes/<topic>.md"));
    assert!(prompt.contains("not replay past turns"));
    assert!(prompt.contains("timestamp-log-like"));
    assert!(prompt.contains("stable user preferences"));
    assert!(prompt.contains("Before long-running work, update Active Context"));
    assert!(prompt.contains("Turn startup sequence:"));
    assert!(prompt.contains("Use history-read or message-search when older channel/thread context"));
    assert!(prompt.contains("Reply briefly to direct greetings"));
    assert!(prompt.contains("Agent context tools"));
    assert!(prompt.contains("inbox-list"));
    assert!(prompt.contains("[target=... msg=... time=... type=...]"));
    assert!(prompt.contains("Live inbox delivery"));
    assert!(prompt.contains("choose only from that item's allowed_actions"));
    assert!(prompt.contains("Persistent memory: prefer concise replies"));
}

#[test]
fn ensure_agent_workspace_creates_index_memory_template_and_notes_dir() {
    let dir = std::env::temp_dir().join(format!("lantor-memory-template-{}", Uuid::new_v4()));
    ensure_agent_workspace(dir.to_str().expect("utf8 temp dir"), "template-agent")
        .expect("ensure workspace");

    let memory = std::fs::read_to_string(dir.join("MEMORY.md")).expect("read memory");
    assert!(dir.join("notes").is_dir());
    assert!(memory.contains("# @template-agent"));
    assert!(memory.contains("## Key Knowledge"));
    assert!(memory.contains("## Memory Map"));
    assert!(memory.contains("notes/user-preferences.md"));
    assert!(memory.contains("notes/work-log.md"));
    assert!(memory.contains("## Active Context"));
    assert!(memory.contains("Keep this file concise and index-like"));
    assert!(memory.contains("Do not use MEMORY.md as a chronological log"));

    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn streaming_prompt_replaces_stdout_finish_contract() {
    let prompt = build_work_item_prompt(
        Uuid::nil(),
        "Review the change",
        "Latest user message: please review",
        Some("lantor"),
        None,
        Some(Uuid::nil()),
        &[],
        None,
    );
    assert!(prompt.contains("Treat messages as conversation"));
    assert!(prompt.contains(WORK_ITEM_FINISH_PROMPT));

    let streaming = build_codex_streaming_prompt(&prompt);
    assert!(streaming.contains("will stream your Codex assistant text"));
    assert!(streaming.contains("Reply briefly to direct greetings"));
    assert!(streaming.contains("pure acknowledgement"));
    assert!(streaming.contains("you may emit standalone LANTOR_EVENT control lines"));
    assert!(streaming.contains("Activity progress: before your final reply"));
    assert!(streaming.contains("activity is not only for reasoning"));
    assert!(streaming.contains("what you are doing or what you just learned"));
    assert!(streaming.contains("artifact_create"));
    assert!(streaming.contains("attachment_create"));
    assert!(streaming.contains("channel_message_create"));
    assert!(streaming.contains("handoff_create"));
    assert!(streaming.contains("task_handoff"));
    assert!(streaming.contains("task_claim"));
    assert!(streaming.contains("choose only from the item's allowed_actions"));
    assert!(streaming.contains("Do not narrate every intermediate step in chat"));
    assert!(!streaming.contains(WORK_ITEM_FINISH_PROMPT));
}

#[test]
fn streaming_work_item_prompt_omits_repeated_standing_context() {
    let prompt = build_streaming_work_item_prompt(
        Uuid::nil(),
        "Review the change",
        "Latest user message: please review",
        Some("lantor"),
        None,
        Some(Uuid::nil()),
        &[],
        None,
    );

    assert!(prompt.contains("Standing instructions are already installed"));
    assert!(prompt.contains("authoritative over older warm-runtime context"));
    assert!(prompt.contains("Same-channel/thread follow-ups may be delivered"));
    assert!(prompt.contains("Latest user message: please review"));
    assert!(prompt.contains(WORK_ITEM_FINISH_PROMPT));
    assert!(!prompt.contains("Operating policy:"));
    assert!(!prompt.contains("Agent context tools:"));
    assert!(!prompt.contains("Standalone LANTOR_EVENT control lines:"));
}

#[test]
fn work_item_prompt_includes_agent_profile_hint_when_present() {
    let prompt = build_work_item_prompt(
        Uuid::nil(),
        "Handle inbox",
        "Latest user message: hello",
        Some("lantor"),
        None,
        Some(Uuid::nil()),
        &[],
        Some("Pick a stable DiceBear avatar if the profile is empty."),
    );

    assert!(prompt.contains("agent_profile_hint:"));
    assert!(prompt.contains("Pick a stable DiceBear avatar if the profile is empty."));
}
