pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod lifecycle;
pub(crate) mod network_retry;
pub(crate) mod process;
pub(crate) mod streaming;
pub(crate) mod supervisor;
pub(crate) mod surface;
pub(crate) mod tools;
pub(crate) mod turn_outcome;
pub(crate) mod warm;

const MEMORY_CONTEXT_CLEARED: &str = "Lantor durable memory update:\nThe previously loaded Lantor memory snapshot has been cleared. Do not rely on facts or preferences from that earlier snapshot unless the current request or current source confirms them.";

fn normalized_memory_context(memory_context: Option<&str>) -> Option<&str> {
    memory_context
        .map(str::trim)
        .filter(|context| !context.is_empty())
}

pub(super) fn runtime_environment_changed(
    current_environment_variables: &str,
    next_environment_variables: &str,
) -> bool {
    current_environment_variables != next_environment_variables
}

pub(super) fn memory_context_update(
    current_memory_context: Option<&str>,
    next_memory_context: Option<&str>,
) -> Option<(String, Option<String>)> {
    let current = normalized_memory_context(current_memory_context);
    let next = normalized_memory_context(next_memory_context);
    if current == next {
        return None;
    }

    match next {
        Some(next) => Some((next.to_owned(), Some(next.to_owned()))),
        None => Some((MEMORY_CONTEXT_CLEARED.to_owned(), None)),
    }
}

#[cfg(test)]
mod tests {
    use super::{memory_context_update, runtime_environment_changed};

    #[test]
    fn runtime_environment_change_requires_restart() {
        assert!(!runtime_environment_changed("", ""));
        assert!(runtime_environment_changed("KEY=old", "KEY=new"));
    }

    #[test]
    fn memory_context_update_only_emits_changes() {
        assert_eq!(memory_context_update(None, None), None);
        assert_eq!(
            memory_context_update(Some("  memory  "), Some("memory")),
            None
        );
        assert_eq!(
            memory_context_update(None, Some(" memory ")),
            Some(("memory".to_owned(), Some("memory".to_owned())))
        );
        assert_eq!(
            memory_context_update(Some("old"), Some("new")),
            Some(("new".to_owned(), Some("new".to_owned())))
        );
        let (cleared_prompt, next) =
            memory_context_update(Some("old"), None).expect("clear update");
        assert!(cleared_prompt.contains("has been cleared"));
        assert_eq!(next, None);
    }
}
