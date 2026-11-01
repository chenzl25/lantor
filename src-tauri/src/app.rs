pub(crate) type CommandResult<T> = Result<T, String>;

pub(crate) fn to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
