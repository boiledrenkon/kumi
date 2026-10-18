//! What the tools do: find the running bridges of one DAW through their discovery files, connect, and report.
use crate::client::{BridgeClient, ConnectError, RequestError};
use daw_bridge::files::{read_secret, scan_discovery, Discovery, FileError};
use daw_bridge::paths::BridgePaths;
use daw_bridge::Daw;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

pub const CLIENT_NAME: &str = concat!("daw-mcp-server/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);

struct Current {
    pid: u32,
    resource: String,
    port: u16,
    client: BridgeClient,
}

/// The host for one DAW.
pub struct Host {
    daw: Daw,
    paths: BridgePaths,
    current: tokio::sync::Mutex<Option<Current>>,
}

/// Which running instance a tool means, when several run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pick {
    pub pid: Option<u32>,
    pub resource: Option<String>,
}

impl Pick {
    pub fn from_arguments(arguments: &Value) -> Result<Self, String> {
        let pid = match arguments.get("pid") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().and_then(|pid| u32::try_from(pid).ok()).ok_or("pid must be a process id")?),
        };
        let resource = match arguments.get("resource") {
            None | Some(Value::Null) => None,
            Some(Value::String(resource)) => Some(resource.clone()),
            Some(_) => return Err("resource must be a folder path".into()),
        };
        Ok(Self { pid, resource })
    }

    fn matches(&self, discovery: &Discovery) -> bool {
        self.pid.is_none_or(|pid| pid == discovery.pid) && self.resource.as_ref().is_none_or(|resource| *resource == discovery.resource)
    }
}

fn connect_error(daw: Daw, error: &ConnectError) -> String {
    match error {
        ConnectError::Handshake(daw_bridge::handshake::HandshakeError::Refused { reason, protocol: Some(protocol) })
            if reason == "protocol" =>
        {
            format!(
                "the bridge in {daw} speaks {}, another version than this Kumi's: restart {daw} to load the installed one",
                daw_bridge::handshake::known_protocol(protocol)
            )
        }
        ConnectError::Handshake(daw_bridge::handshake::HandshakeError::Refused { reason, .. }) if reason == "auth" => {
            format!("the bridge in {daw} was set up with another secret: restart {daw}, or set up Kumi's bridge again")
        }
        ConnectError::Handshake(daw_bridge::handshake::HandshakeError::Refused { reason, .. }) if reason == "connections" => {
            format!("the bridge in {daw} already has as many connections as it takes: close another Kumi or MCP app connected to it")
        }
        ConnectError::Handshake(daw_bridge::handshake::HandshakeError::BadProof) => {
            format!("whatever listens there isn't Kumi's bridge, or {daw} was set up with another secret")
        }
        other => other.to_string(),
    }
}

/// A connect that nothing answers: refused, or (Windows retries the SYN to a closed loopback port for about 2 s before
/// it refuses) not accepted in time. A running bridge accepts at once, even while its DAW is busy, so either means
/// the process is gone.
fn no_listener(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut)
}

impl Host {
    pub fn new(daw: Daw, kumi_home: &Path) -> Self {
        Self { daw, paths: BridgePaths::new(kumi_home, daw), current: tokio::sync::Mutex::new(None) }
    }

    pub fn daw(&self) -> Daw {
        self.daw
    }

    /// The running instances (their pids alive), newest first, and how many stale files were removed.
    fn instances(&self) -> (Vec<Discovery>, usize, Vec<String>) {
        let mut stale = 0;
        let mut invalid = vec![];
        let mut alive = vec![];
        for found in scan_discovery(&self.paths.discovery(), self.daw) {
            match (found.discovery, found.alive) {
                (_, false) => {
                    if std::fs::remove_file(&found.path).is_ok() {
                        stale += 1;
                    }
                }
                (Ok(discovery), true) => alive.push(discovery),
                (Err(why), true) => invalid.push(format!("{}: {why}", found.path.file_name().unwrap_or_default().to_string_lossy())),
            }
        }
        (alive, stale, invalid)
    }

    fn secret(&self) -> Result<daw_bridge::auth::Secret, String> {
        read_secret(&self.paths.secret()).map_err(|error| match error {
            FileError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                format!("Kumi's bridge for {} isn't set up yet (no secret)", self.daw)
            }
            FileError::NotOwnerOnly => format!("the secret for Kumi's {} bridge is readable by other accounts", self.daw),
            other => format!("the secret for Kumi's {} bridge can't be read: {other}", self.daw),
        })
    }

    /// Connects to the instance a discovery file names. A file without a pid (Renoise on Windows) whose port nobody
    /// answers is stale: it's removed, and the result is None.
    async fn connect(&self, discovery: &Discovery, secret: &daw_bridge::auth::Secret) -> Option<Result<BridgeClient, String>> {
        let address = SocketAddr::from(([127, 0, 0, 1], discovery.port));
        let client = match BridgeClient::connect_within(address, secret.clone(), CLIENT_NAME, CONNECT_TIMEOUT).await {
            Ok(client) => client,
            Err(ConnectError::Io(error)) if discovery.pid == 0 && no_listener(&error) => {
                let _ = std::fs::remove_file(self.paths.discovery().join(discovery.file_name()));
                return None;
            }
            Err(error) => return Some(Err(connect_error(self.daw, &error))),
        };
        let identity = &client.welcome().identity;
        if identity.daw != self.daw.id() || identity.pid != discovery.pid || identity.resource != discovery.resource {
            client.close().await;
            let daw = daw_bridge::handshake::tidy(&identity.daw, 16);
            return Some(Err(format!("port {} answered as another instance ({daw} pid {})", discovery.port, identity.pid)));
        }
        Some(Ok(client))
    }

    /// `discover`: every running instance's bridge, connected and asked for its status.
    pub async fn discover(&self) -> Value {
        let (alive, mut stale, invalid) = self.instances();
        let secret = self.secret();
        let mut instances = vec![];
        for discovery in &alive {
            let mut row = json!({
                "pid": discovery.pid,
                "port": discovery.port,
                "resource": discovery.resource,
                "bridge": discovery.version,
                "dawVersion": discovery.daw_version,
                "project": discovery.project,
            });
            match &secret {
                Err(why) => {
                    row["connected"] = false.into();
                    row["error"] = why.clone().into();
                }
                Ok(secret) => match self.connect(discovery, secret).await {
                    None => {
                        stale += 1;
                        continue;
                    }
                    Some(Err(why)) => {
                        row["connected"] = false.into();
                        row["error"] = why.into();
                    }
                    Some(Ok(client)) => {
                        row["connected"] = true.into();
                        row["bridge"] = client.welcome().identity.bridge.clone().into();
                        match client.request("status", json!({}), STATUS_TIMEOUT).await {
                            Ok(status) => {
                                row["project"] = status["project"].clone();
                                row["busy"] = status["busy"].clone();
                                if let Some(demo) = status.get("demo") {
                                    row["demo"] = demo.clone();
                                }
                            }
                            Err(error) => row["busy"] = busy_word(&error).into(),
                        }
                        client.close().await;
                    }
                },
            }
            instances.push(row);
        }
        let mut result = json!({"daw": self.daw.id(), "setUp": secret.is_ok(), "instances": instances});
        if stale > 0 {
            result["staleRemoved"] = stale.into();
        }
        if !invalid.is_empty() {
            result["invalid"] = invalid.into();
        }
        if instances.is_empty() {
            result["hint"] = match &secret {
                Err(why) => why.clone(),
                Ok(_) => format!(
                    "no {} with Kumi's bridge is running: open {}, or restart it if it was open before Kumi set up its bridge",
                    self.daw, self.daw
                ),
            }
            .into();
        }
        result
    }

    /// `status`: the chosen instance's bridge (the one already connected, else the only one or the newest).
    pub async fn status(&self, pick: &Pick) -> Value {
        let mut current = self.current.lock().await;
        let reusable = current.as_ref().is_some_and(|open| {
            !open.client.is_closed()
                && pick.pid.is_none_or(|pid| pid == open.pid)
                && pick.resource.as_ref().is_none_or(|r| *r == open.resource)
        });
        let (alive, _, _) = self.instances();
        if !reusable {
            if let Some(open) = current.take() {
                open.client.close().await;
            }
            let secret = match self.secret() {
                Ok(secret) => secret,
                Err(why) => return self.not_connected(why),
            };
            let Some(discovery) = alive.iter().find(|discovery| pick.matches(discovery)) else {
                let why = if alive.is_empty() {
                    format!("no {} with Kumi's bridge is running", self.daw)
                } else {
                    format!("no running {} matches that pid or folder", self.daw)
                };
                return self.not_connected(why);
            };
            match self.connect(discovery, &secret).await {
                Some(Ok(client)) => {
                    *current = Some(Current { pid: discovery.pid, resource: discovery.resource.clone(), port: discovery.port, client });
                }
                Some(Err(why)) => return self.not_connected(why),
                None => return self.not_connected(format!("no {} with Kumi's bridge is running", self.daw)),
            }
        }
        let open = current.as_ref().expect("connected above");
        let identity = &open.client.welcome().identity;
        let mut result = json!({
            "connected": true,
            "daw": self.daw.id(),
            "bridge": identity.bridge,
            "dawVersion": identity.daw_version,
            "pid": open.pid,
            "port": open.port,
            "resource": open.resource,
            "project": identity.project,
            "busy": false,
            "instances": alive.len().max(1),
        });
        match open.client.request("status", json!({}), STATUS_TIMEOUT).await {
            Ok(status) => {
                result["project"] = status["project"].clone();
                result["busy"] = status.get("busy").cloned().unwrap_or(Value::Bool(false));
                if let Some(demo) = status.get("demo") {
                    result["demo"] = demo.clone();
                }
            }
            Err(RequestError::Closed) => {
                current.take();
                return self.not_connected(format!("{} closed the connection", self.daw));
            }
            Err(error) => result["busy"] = busy_word(&error).into(),
        }
        result
    }

    fn not_connected(&self, reason: String) -> Value {
        json!({"connected": false, "daw": self.daw.id(), "reason": reason})
    }

    pub async fn close(&self) {
        if let Some(open) = self.current.lock().await.take() {
            open.client.close().await;
        }
    }
}

fn busy_word(error: &RequestError) -> String {
    match error {
        RequestError::Bridge { code, message } if code == daw_bridge::wire::code::BUSY => message.clone(),
        RequestError::Timeout => "not answering (a dialog may be open)".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn a_refused_or_unanswered_connect_means_nothing_listens() {
        assert!(no_listener(&Error::from(ErrorKind::ConnectionRefused)));
        assert!(no_listener(&Error::from(ErrorKind::TimedOut)));
        assert!(!no_listener(&Error::from(ErrorKind::ConnectionReset)));
        assert!(!no_listener(&Error::from(ErrorKind::PermissionDenied)));
    }
}
