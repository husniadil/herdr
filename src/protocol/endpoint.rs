//! Stable endpoint compatibility contract for client-owned shells.
//!
//! The endpoint generation is intentionally independent from the private
//! binary protocol used by same-install CLI, direct-terminal, and handoff
//! paths. Generation 1 is the compatibility floor for Local, SSH, and Cloud
//! shell endpoints and must remain available indefinitely unless retired for a
//! security reason. New JSON fields must be optional or have serde defaults;
//! new enum values need an `Unknown` fallback. Unknown named controls are
//! optional and ignored unless negotiated as part of the core.

use serde::{Deserialize, Serialize};

use super::{ClientMessage, ClientShellSnapshot, ClientSurfaceSize, ServerMessage};

pub const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;
pub const ENDPOINT_HELLO_KIND: &str = "endpoint.hello.v1";
pub const ENDPOINT_WELCOME_KIND: &str = "endpoint.welcome.v1";
pub const SNAPSHOT_CODEC_V1: &str = "shell.snapshot.v1";
pub const ENDPOINT_SNAPSHOT_KIND: &str = SNAPSHOT_CODEC_V1;
pub const SURFACE_CODEC_V1: &str = "shell.surface.v1";
pub const INPUT_CODEC_V1: &str = "shell.input.semantic.v1";
pub const BLOB_CODEC_V1: &str = "shell.blob.v1";
pub const SURFACE_INTEREST_CAPABILITY: &str = "surface_interest";
pub const PRESENTATION_EFFECTS_FENCE_CAPABILITY: &str = "presentation_effects_fence";
pub const PRESENTATION_EFFECTS_SYNC_KIND: &str = "endpoint.presentation.sync.v1";
pub const PRESENTATION_EFFECTS_READY_KIND: &str = "endpoint.presentation.ready.v1";
pub const HEALTH_CHECK_CAPABILITY: &str = "health_check";
pub const HEALTH_PING_KIND: &str = "endpoint.health.ping.v1";
pub const HEALTH_PONG_KIND: &str = "endpoint.health.pong.v1";
/// The server honours `workspace_id` and `client_tag` in the hello and serves the
/// per-client view methods (`client.list`, `client.view.focus`).
pub const CLIENT_VIEW_FOCUS_CAPABILITY: &str = "client_view_focus";
/// The server records `endpoint.snapshot.applied.v1` acknowledgements from clients whose
/// hello set `snapshot_acks`, reports them in `client.list`, and serves `client.view.wait`.
pub const CLIENT_VIEW_ACK_CAPABILITY: &str = "client_view_ack";
/// Client to server: the snapshot a client now routes input through. Data is
/// [`EndpointSnapshotApplied`] as JSON.
pub const SNAPSHOT_APPLIED_KIND: &str = "endpoint.snapshot.applied.v1";
/// Longest client tag a server accepts, in bytes.
pub const CLIENT_TAG_MAX_BYTES: usize = 128;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointClientHello {
    pub generation: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub surface_size: ClientSurfaceSize,
    pub pixel_mouse: bool,
    pub direct_graphics: bool,
    pub endpoint_keybindings: bool,
    pub mouse_capture: bool,
    #[serde(default = "default_true")]
    pub surface_active: bool,
    #[serde(default)]
    pub snapshot_codecs: Vec<String>,
    #[serde(default)]
    pub surface_codecs: Vec<String>,
    #[serde(default)]
    pub input_codecs: Vec<String>,
    #[serde(default)]
    pub blob_codecs: Vec<String>,
    /// Workspace this connection should show first. A server without
    /// `client_view_focus` ignores it and shows its own focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Caller-chosen label that addresses this connection in `client.list` and
    /// `client.view.focus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_tag: Option<String>,
    /// This client sends [`SNAPSHOT_APPLIED_KIND`] after it applies a snapshot, when
    /// the server advertises [`CLIENT_VIEW_ACK_CAPABILITY`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub snapshot_acks: bool,
}

/// A client's acknowledgement that the snapshot with this boot and revision is the
/// one its input is routed through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointSnapshotApplied {
    pub boot_id: String,
    pub revision: u64,
}

pub fn snapshot_applied_message(boot_id: &str, revision: u64) -> serde_json::Result<ClientMessage> {
    Ok(ClientMessage::EndpointControl {
        kind: SNAPSHOT_APPLIED_KIND.into(),
        data: serde_json::to_string(&EndpointSnapshotApplied {
            boot_id: boot_id.to_owned(),
            revision,
        })?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHandshakeError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointServerWelcome {
    pub generation: u32,
    pub server_version: String,
    pub snapshot_codec: String,
    pub surface_codec: String,
    pub input_codec: String,
    pub blob_codec: String,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EndpointHandshakeError>,
}

pub fn snapshot_message(snapshot: &ClientShellSnapshot) -> serde_json::Result<ServerMessage> {
    Ok(ServerMessage::EndpointControl {
        kind: ENDPOINT_SNAPSHOT_KIND.into(),
        data: serde_json::to_string(snapshot)?,
    })
}

/// Checks a caller-chosen client tag: non-empty, at most
/// [`CLIENT_TAG_MAX_BYTES`], and free of control characters so it prints and
/// matches predictably.
pub fn validate_client_tag(tag: &str) -> Result<(), String> {
    if tag.is_empty() {
        return Err("client tag is empty".into());
    }
    if tag.len() > CLIENT_TAG_MAX_BYTES {
        return Err(format!(
            "client tag is longer than {CLIENT_TAG_MAX_BYTES} bytes"
        ));
    }
    if tag.chars().any(char::is_control) {
        return Err("client tag contains a control character".into());
    }
    Ok(())
}

impl EndpointClientHello {
    pub fn supports_required_codecs(&self) -> bool {
        self.snapshot_codecs
            .iter()
            .any(|codec| codec == SNAPSHOT_CODEC_V1)
            && self
                .surface_codecs
                .iter()
                .any(|codec| codec == SURFACE_CODEC_V1)
            && self
                .input_codecs
                .iter()
                .any(|codec| codec == INPUT_CODEC_V1)
            && self.blob_codecs.iter().any(|codec| codec == BLOB_CODEC_V1)
    }
}

impl EndpointServerWelcome {
    pub fn compatible(methods: Vec<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods,
            capabilities: vec![
                SURFACE_INTEREST_CAPABILITY.into(),
                PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                HEALTH_CHECK_CAPABILITY.into(),
                CLIENT_VIEW_FOCUS_CAPABILITY.into(),
                CLIENT_VIEW_ACK_CAPABILITY.into(),
            ],
            error: None,
        }
    }

    pub fn incompatible(code: &str, message: impl Into<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods: Vec::new(),
            capabilities: Vec::new(),
            error: Some(EndpointHandshakeError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> EndpointClientHello {
        EndpointClientHello {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: ClientSurfaceSize { cols: 80, rows: 24 },
            pixel_mouse: true,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
            snapshot_codecs: vec![SNAPSHOT_CODEC_V1.into()],
            surface_codecs: vec![SURFACE_CODEC_V1.into()],
            input_codecs: vec![INPUT_CODEC_V1.into()],
            blob_codecs: vec![BLOB_CODEC_V1.into()],
            workspace_id: None,
            client_tag: None,
            snapshot_acks: false,
        }
    }

    fn snapshot() -> ClientShellSnapshot {
        ClientShellSnapshot {
            boot_id: "boot".into(),
            revision: 1,
            config_diagnostic: None,
            product_announcement: None,
            update_available: None,
            update_install_command: "herdr update".into(),
            server_keybindings_toml: None,
            latest_release_notes_available: false,
            integration_updates_available: false,
            worktree_directory: String::new(),
            release_notes: None,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: String::new(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            agents: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn hello_ignores_future_named_fields() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value["future_feature"] = serde_json::json!({"enabled": true});
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, hello());
    }

    #[test]
    fn frozen_generation_one_handshake_decodes() {
        let hello: EndpointClientHello = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-hello-v1.json"
        )))
        .unwrap();
        assert!(hello.supports_required_codecs());

        let welcome: EndpointServerWelcome = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-welcome-v1.json"
        )))
        .unwrap();
        assert_eq!(welcome.generation, ENDPOINT_PROTOCOL_GENERATION);
        assert_eq!(welcome.snapshot_codec, SNAPSHOT_CODEC_V1);
        assert_eq!(welcome.surface_codec, SURFACE_CODEC_V1);
        assert_eq!(welcome.input_codec, INPUT_CODEC_V1);
        assert_eq!(welcome.blob_codec, BLOB_CODEC_V1);
        assert!(welcome.capabilities.is_empty());
    }

    #[test]
    fn frozen_generation_one_snapshot_decodes() {
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-snapshot-v1.json"
        )))
        .unwrap();
        assert_eq!(snapshot.boot_id, "boot-v1");
        assert_eq!(
            snapshot.workspaces[0].agent_status,
            crate::api::schema::AgentStatus::Unknown
        );
    }

    #[test]
    fn snapshot_message_uses_named_json_control() {
        let snapshot = snapshot();
        let ServerMessage::EndpointControl { kind, data } = snapshot_message(&snapshot).unwrap()
        else {
            panic!("snapshot should use endpoint control");
        };
        assert_eq!(kind, ENDPOINT_SNAPSHOT_KIND);
        let decoded: ClientShellSnapshot = serde_json::from_str(&data).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn snapshot_json_tolerates_future_fields_and_command_actions() {
        let mut snapshot = match snapshot_message(&snapshot()).unwrap() {
            ServerMessage::EndpointControl { data, .. } => {
                serde_json::from_str::<serde_json::Value>(&data).unwrap()
            }
            _ => unreachable!(),
        };
        snapshot["future_projection"] = serde_json::json!({"enabled": true});
        snapshot["commands"] = serde_json::json!([{
            "command_id": "future",
            "binding_label": "x",
            "binding_labels": ["x"],
            "action": "FutureAction",
            "description": null
        }]);

        let decoded: ClientShellSnapshot = serde_json::from_value(snapshot).unwrap();
        assert_eq!(
            decoded.commands[0].action,
            crate::protocol::ClientShellCommandAction::Unknown
        );
    }

    #[test]
    fn legacy_hello_defaults_to_an_active_surface() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value.as_object_mut().unwrap().remove("surface_active");
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert!(decoded.surface_active);
    }

    #[test]
    fn compatible_server_advertises_endpoint_lifecycle_capabilities() {
        let welcome = EndpointServerWelcome::compatible(Vec::new());
        assert_eq!(
            welcome.capabilities,
            vec![
                SURFACE_INTEREST_CAPABILITY.to_string(),
                PRESENTATION_EFFECTS_FENCE_CAPABILITY.to_string(),
                HEALTH_CHECK_CAPABILITY.to_string(),
                CLIENT_VIEW_FOCUS_CAPABILITY.to_string(),
                CLIENT_VIEW_ACK_CAPABILITY.to_string(),
            ]
        );
    }

    #[test]
    fn hello_without_snapshot_acks_serializes_as_before_and_decodes_as_false() {
        let value = serde_json::to_value(hello()).unwrap();
        assert!(value.get("snapshot_acks").is_none());
        let frozen: EndpointClientHello = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-hello-v1.json"
        )))
        .unwrap();
        assert!(!frozen.snapshot_acks);

        let mut value = value;
        value["snapshot_acks"] = serde_json::json!(true);
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert!(decoded.snapshot_acks);
    }

    #[test]
    fn snapshot_applied_uses_a_named_json_control() {
        let ClientMessage::EndpointControl { kind, data } =
            snapshot_applied_message("boot", 7).unwrap()
        else {
            panic!("snapshot acknowledgement should use endpoint control");
        };
        assert_eq!(kind, SNAPSHOT_APPLIED_KIND);
        assert_eq!(
            serde_json::from_str::<EndpointSnapshotApplied>(&data).unwrap(),
            EndpointSnapshotApplied {
                boot_id: "boot".into(),
                revision: 7,
            }
        );
    }

    #[test]
    fn hello_without_view_fields_serializes_as_before() {
        let value = serde_json::to_value(hello()).unwrap();
        assert!(value.get("workspace_id").is_none());
        assert!(value.get("client_tag").is_none());
    }

    #[test]
    fn hello_carries_optional_workspace_and_client_tag() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value["workspace_id"] = serde_json::json!("w_2");
        value["client_tag"] = serde_json::json!("browser-1");
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.workspace_id.as_deref(), Some("w_2"));
        assert_eq!(decoded.client_tag.as_deref(), Some("browser-1"));
    }

    #[test]
    fn client_tag_validation_rejects_empty_long_and_control_text() {
        assert!(validate_client_tag("browser-1").is_ok());
        assert!(validate_client_tag("").is_err());
        assert!(validate_client_tag(&"x".repeat(CLIENT_TAG_MAX_BYTES + 1)).is_err());
        assert!(validate_client_tag(&"x".repeat(CLIENT_TAG_MAX_BYTES)).is_ok());
        assert!(validate_client_tag("tab\u{1b}").is_err());
    }

    #[test]
    fn required_codecs_are_explicit() {
        let mut value = hello();
        assert!(value.supports_required_codecs());
        value.snapshot_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.surface_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.input_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.blob_codecs.clear();
        assert!(!value.supports_required_codecs());
    }

    #[test]
    fn welcome_ignores_future_named_fields() {
        let welcome = EndpointServerWelcome::compatible(vec!["pane.close".into()]);
        let mut value = serde_json::to_value(&welcome).unwrap();
        value["future_service"] = serde_json::json!("v2");
        let decoded: EndpointServerWelcome = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, welcome);
    }
}
