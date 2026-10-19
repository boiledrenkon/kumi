//! Kumi's host (daw-mcp-server's own code) as an MCP endpoint called in process, for tests of the own DAWs'
//! integrations: what the host answers is what the integration reads.
use async_trait::async_trait;
use daw_mcp_server::host::Host;
use kumi_common::abort::Signal;
use kumi_runtime::core::{contracts::JsonObject, errors::RuntimeError};
use kumi_runtime::mcp::client::{McpEndpoint, StderrStatus};
use kumi_runtime::mcp::types::{CallToolResult, Implementation, ListToolsResult};
use serde_json::json;
use std::{cell::Cell, rc::Rc, sync::Arc};

/// The host's MCP, called in process.
pub struct InProcess {
    pub host: Arc<Host>,
    pub id: Cell<u64>,
}
#[async_trait(?Send)]
impl McpEndpoint for InProcess {
    fn pid(&self) -> Option<u32> {
        None
    }
    fn server_info(&self) -> Option<Implementation> {
        None
    }
    async fn list(&self, _: Option<&str>, _: Signal) -> Result<ListToolsResult, RuntimeError> {
        Ok(serde_json::from_value(json!({"tools": daw_mcp_server::mcp::tools(&self.host)})).unwrap())
    }
    async fn call(&self, name: &str, args: JsonObject, _: Signal) -> Result<CallToolResult, RuntimeError> {
        self.id.set(self.id.get() + 1);
        let message = json!({"jsonrpc": "2.0", "id": self.id.get(), "method": "tools/call", "params": {"name": name, "arguments": args}});
        let answer = daw_mcp_server::mcp::handle(&self.host, &message).await.expect("an answer");
        Ok(serde_json::from_value(answer["result"].clone()).unwrap_or_else(|e| panic!("{answer}: {e}")))
    }
    fn on_catalog_changed(&self, _: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        Box::new(|| {})
    }
    fn on_disconnect(&self, _: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        Box::new(|| {})
    }
    /// The bridge's events as the host forwards them (`notifications/daw_event`).
    fn on_live_event(&self, listener: Rc<dyn Fn(JsonObject)>) -> Box<dyn Fn()> {
        let mut events = self.host.events();
        let task = tokio::task::spawn_local(async move {
            while let Ok(event) = events.recv().await {
                if let Some(event) = event.as_object() {
                    listener(event.clone());
                }
            }
        });
        Box::new(move || task.abort())
    }
    fn stderr_status(&self) -> StderrStatus {
        StderrStatus { bytes: 0, truncated: false }
    }
    async fn close(&self) -> Result<(), RuntimeError> {
        self.host.close().await;
        Ok(())
    }
}
