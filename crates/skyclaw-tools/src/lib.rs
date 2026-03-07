//! SkyClaw Tools — agent capabilities (shell, file, web, browser, etc.)

mod shell;
mod file;
mod web_fetch;
mod send_file;
mod send_message;

pub use shell::ShellTool;
pub use file::{FileReadTool, FileWriteTool, FileListTool};
pub use web_fetch::WebFetchTool;
pub use send_file::SendFileTool;
pub use send_message::SendMessageTool;

use std::sync::Arc;
use skyclaw_core::{Channel, Tool};
use skyclaw_core::types::config::ToolsConfig;

/// Create tools based on the configuration flags.
/// Pass an optional channel for file transfer tools.
pub fn create_tools(config: &ToolsConfig, channel: Option<Arc<dyn Channel>>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    if config.shell {
        tools.push(Arc::new(ShellTool::new()));
    }

    if config.file {
        tools.push(Arc::new(FileReadTool::new()));
        tools.push(Arc::new(FileWriteTool::new()));
        tools.push(Arc::new(FileListTool::new()));
    }

    if config.http {
        tools.push(Arc::new(WebFetchTool::new()));
    }

    // Add channel-dependent tools
    if let Some(ch) = channel {
        // send_message: send intermediate text messages during tool execution
        tools.push(Arc::new(SendMessageTool::new(ch.clone())));

        // send_file: send files if channel supports file transfer
        if ch.file_transfer().is_some() {
            tools.push(Arc::new(SendFileTool::new(ch)));
        }
    }

    tracing::info!(count = tools.len(), "Tools registered");
    tools
}
