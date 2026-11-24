use tokio::process::Command;

use crate::app::CommandResult;

#[derive(Debug, Eq, PartialEq)]
struct AgentEnvironmentVariable {
    key: String,
    value: String,
}

fn valid_environment_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn normalize_environment_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_owned();
        }
    }
    value.to_owned()
}

fn parse_agent_environment_variables(
    environment_variables: &str,
) -> CommandResult<Vec<AgentEnvironmentVariable>> {
    let mut variables = Vec::new();
    for (index, raw_line) in environment_variables.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "invalid environment variable on line {}: expected KEY=VALUE",
                index + 1
            ));
        };
        let key = key.trim();
        if !valid_environment_key(key) {
            return Err(format!(
                "invalid environment variable name on line {}: {key}",
                index + 1
            ));
        }
        variables.push(AgentEnvironmentVariable {
            key: key.to_owned(),
            value: normalize_environment_value(value),
        });
    }
    Ok(variables)
}

pub(crate) fn normalize_agent_environment_variables(
    environment_variables: Option<&str>,
) -> CommandResult<String> {
    let variables = parse_agent_environment_variables(environment_variables.unwrap_or_default())?;
    Ok(variables
        .into_iter()
        .map(|variable| format!("{}={}", variable.key, variable.value))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The value an agent's environment sets for `key`. A later line wins, as it
/// does when the variables are applied to the process.
pub(crate) fn agent_environment_value(environment_variables: &str, key: &str) -> Option<String> {
    parse_agent_environment_variables(environment_variables)
        .ok()?
        .into_iter()
        .rev()
        .find(|variable| variable.key == key)
        .map(|variable| variable.value)
}

pub(crate) fn apply_agent_environment_variables(
    command: &mut Command,
    environment_variables: &str,
) -> CommandResult<()> {
    for variable in parse_agent_environment_variables(environment_variables)? {
        command.env(variable.key, variable.value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalize_agent_environment_variables;

    #[test]
    fn normalizes_agent_environment_variables() {
        assert_eq!(
            normalize_agent_environment_variables(Some(
                r#"
                # comment
                HTTP_PROXY=http://127.0.0.1:7897
                export NO_PROXY="localhost,127.0.0.1"
                EMPTY=
                "#
            ))
            .unwrap(),
            "HTTP_PROXY=http://127.0.0.1:7897\nNO_PROXY=localhost,127.0.0.1\nEMPTY="
        );
    }

    #[test]
    fn rejects_invalid_agent_environment_variables() {
        let err = normalize_agent_environment_variables(Some("1BAD=value")).unwrap_err();
        assert!(err.contains("line 1"));
        assert!(err.contains("1BAD"));

        let err = normalize_agent_environment_variables(Some("MISSING_VALUE")).unwrap_err();
        assert!(err.contains("expected KEY=VALUE"));
    }
}
