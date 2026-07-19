use std::path::Path;

use crate::AgentError;

#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the native instruction-loader contract"
)]
pub(super) fn load_project_instructions(_workspace: &Path) -> Result<Option<String>, AgentError> {
    Ok(None)
}
