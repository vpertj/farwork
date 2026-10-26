use serde::{Deserialize, Serialize};

use crate::device::{DeviceInfo, DeviceView};

/// Frames sent by a connector to the server on `/ws/connector`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectorToServer {
    Hello {
        device: DeviceInfo,
    },
    #[serde(rename = "terminal.opened")]
    TerminalOpened {
        stream_id: String,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        stream_id: String,
        data: String,
    },
    #[serde(rename = "terminal.exit")]
    TerminalExit {
        stream_id: String,
        code: i32,
    },
    Ping,
    Pong,
}

/// Frames sent by the server to a connector on `/ws/connector`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToConnector {
    Welcome {
        server_version: String,
    },
    #[serde(rename = "terminal.open")]
    TerminalOpen {
        stream_id: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        stream_id: String,
        data: String,
    },
    /// Internal server->connector frame: the published browser contract has no
    /// per-stream resize on the connector channel, so the server forwards the
    /// browser's resize here instead.
    #[serde(rename = "terminal.resize")]
    TerminalResize {
        stream_id: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal.close")]
    TerminalClose {
        stream_id: String,
    },
    Ping,
    Pong,
}

/// Frames sent by a browser client to the server on `/ws/client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientToServer {
    #[serde(rename = "devices.watch")]
    DevicesWatch,
    #[serde(rename = "terminal.open")]
    TerminalOpen {
        device_id: String,
        stream_id: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal.input")]
    TerminalInput { stream_id: String, data: String },
    #[serde(rename = "terminal.resize")]
    TerminalResize {
        stream_id: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal.close")]
    TerminalClose { stream_id: String },
}

/// Frames sent by the server to a browser client on `/ws/client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToClient {
    Devices {
        devices: Vec<DeviceView>,
    },
    #[serde(rename = "terminal.opened")]
    TerminalOpened {
        stream_id: String,
    },
    #[serde(rename = "terminal.error")]
    TerminalError {
        stream_id: String,
        message: String,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        stream_id: String,
        data: String,
    },
    #[serde(rename = "terminal.exit")]
    TerminalExit {
        stream_id: String,
        code: i32,
    },
}

macro_rules! wire_codec {
    ($($message:ty),+ $(,)?) => {
        $(
            impl $message {
                /// Render this frame as the JSON text sent on the wire.
                pub fn to_json(&self) -> String {
                    serde_json::to_string(self).expect("wire frames always serialize")
                }

                /// Parse a JSON text frame received from the wire.
                pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
                    serde_json::from_str(json)
                }
            }
        )+
    };
}

wire_codec!(
    ConnectorToServer,
    ServerToConnector,
    ClientToServer,
    ServerToClient
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Platform;

    fn device() -> DeviceInfo {
        DeviceInfo {
            id: "6f8c1f0e-0000-4000-8000-000000000001".to_string(),
            name: "mac-mini".to_string(),
            platform: Platform::Macos,
        }
    }

    #[test]
    fn hello_frame_matches_contract() {
        let message = ConnectorToServer::Hello { device: device() };
        assert_eq!(
            message.to_json(),
            r#"{"type":"hello","device":{"id":"6f8c1f0e-0000-4000-8000-000000000001","name":"mac-mini","platform":"macos"}}"#
        );
        assert_eq!(
            ConnectorToServer::parse(&message.to_json()).unwrap(),
            message
        );
    }

    #[test]
    fn welcome_and_terminal_open_match_contract() {
        let welcome = ServerToConnector::Welcome {
            server_version: "0.1.0".to_string(),
        };
        assert_eq!(
            welcome.to_json(),
            r#"{"type":"welcome","server_version":"0.1.0"}"#
        );

        let open = ServerToConnector::TerminalOpen {
            stream_id: "s-1".to_string(),
            cols: 80,
            rows: 24,
        };
        assert_eq!(
            open.to_json(),
            r#"{"type":"terminal.open","stream_id":"s-1","cols":80,"rows":24}"#
        );
        assert_eq!(ServerToConnector::parse(&open.to_json()).unwrap(), open);
    }

    #[test]
    fn client_terminal_frames_match_contract() {
        let open = ClientToServer::TerminalOpen {
            device_id: "d-1".to_string(),
            stream_id: "s-1".to_string(),
            cols: 80,
            rows: 24,
        };
        assert_eq!(
            open.to_json(),
            r#"{"type":"terminal.open","device_id":"d-1","stream_id":"s-1","cols":80,"rows":24}"#
        );

        let input = ClientToServer::TerminalInput {
            stream_id: "s-1".to_string(),
            data: "ZWNobyBoaQo=".to_string(),
        };
        assert_eq!(
            input.to_json(),
            r#"{"type":"terminal.input","stream_id":"s-1","data":"ZWNobyBoaQo="}"#
        );

        let resize = ClientToServer::TerminalResize {
            stream_id: "s-1".to_string(),
            cols: 100,
            rows: 30,
        };
        assert_eq!(
            resize.to_json(),
            r#"{"type":"terminal.resize","stream_id":"s-1","cols":100,"rows":30}"#
        );

        assert_eq!(
            ClientToServer::DevicesWatch.to_json(),
            r#"{"type":"devices.watch"}"#
        );
        assert_eq!(
            ClientToServer::TerminalClose {
                stream_id: "s-1".to_string()
            }
            .to_json(),
            r#"{"type":"terminal.close","stream_id":"s-1"}"#
        );
    }

    #[test]
    fn server_to_client_frames_match_contract() {
        let devices = ServerToClient::Devices {
            devices: vec![DeviceView {
                id: "d-1".to_string(),
                name: "mac-mini".to_string(),
                platform: Platform::Macos,
                connected: true,
                last_seen_ms: 1_700_000_000_000,
            }],
        };
        assert_eq!(
            devices.to_json(),
            r#"{"type":"devices","devices":[{"id":"d-1","name":"mac-mini","platform":"macos","connected":true,"last_seen_ms":1700000000000}]}"#
        );

        let error = ServerToClient::TerminalError {
            stream_id: "s-1".to_string(),
            message: "device d-1 is not connected".to_string(),
        };
        assert_eq!(
            error.to_json(),
            r#"{"type":"terminal.error","stream_id":"s-1","message":"device d-1 is not connected"}"#
        );
    }

    #[test]
    fn ping_pong_frames_are_symmetric() {
        assert_eq!(ServerToConnector::Ping.to_json(), r#"{"type":"ping"}"#);
        assert_eq!(ServerToConnector::Pong.to_json(), r#"{"type":"pong"}"#);
        assert_eq!(ConnectorToServer::Ping.to_json(), r#"{"type":"ping"}"#);
        assert_eq!(ConnectorToServer::Pong.to_json(), r#"{"type":"pong"}"#);
        assert_eq!(
            ConnectorToServer::parse(r#"{"type":"pong"}"#).unwrap(),
            ConnectorToServer::Pong
        );
    }

    #[test]
    fn connector_frames_round_trip() {
        let frames = vec![
            ConnectorToServer::Hello { device: device() },
            ConnectorToServer::TerminalOpened {
                stream_id: "s-1".to_string(),
            },
            ConnectorToServer::TerminalData {
                stream_id: "s-1".to_string(),
                data: "aGk=".to_string(),
            },
            ConnectorToServer::TerminalExit {
                stream_id: "s-1".to_string(),
                code: 0,
            },
            ConnectorToServer::Ping,
            ConnectorToServer::Pong,
        ];
        for frame in frames {
            assert_eq!(ConnectorToServer::parse(&frame.to_json()).unwrap(), frame);
        }
    }

    #[test]
    fn server_to_connector_frames_round_trip() {
        let frames = vec![
            ServerToConnector::Welcome {
                server_version: "0.1.0".to_string(),
            },
            ServerToConnector::TerminalOpen {
                stream_id: "s-1".to_string(),
                cols: 80,
                rows: 24,
            },
            ServerToConnector::TerminalData {
                stream_id: "s-1".to_string(),
                data: "aGk=".to_string(),
            },
            ServerToConnector::TerminalResize {
                stream_id: "s-1".to_string(),
                cols: 100,
                rows: 30,
            },
            ServerToConnector::TerminalClose {
                stream_id: "s-1".to_string(),
            },
            ServerToConnector::Ping,
            ServerToConnector::Pong,
        ];
        for frame in frames {
            assert_eq!(ServerToConnector::parse(&frame.to_json()).unwrap(), frame);
        }
    }

    #[test]
    fn server_to_client_frames_round_trip() {
        let frames = vec![
            ServerToClient::Devices { devices: vec![] },
            ServerToClient::TerminalOpened {
                stream_id: "s-1".to_string(),
            },
            ServerToClient::TerminalError {
                stream_id: "s-1".to_string(),
                message: "boom".to_string(),
            },
            ServerToClient::TerminalData {
                stream_id: "s-1".to_string(),
                data: "aGk=".to_string(),
            },
            ServerToClient::TerminalExit {
                stream_id: "s-1".to_string(),
                code: 1,
            },
        ];
        for frame in frames {
            assert_eq!(ServerToClient::parse(&frame.to_json()).unwrap(), frame);
        }
    }

    #[test]
    fn unknown_message_type_is_rejected() {
        assert!(ClientToServer::parse(r#"{"type":"nope"}"#).is_err());
        assert!(ConnectorToServer::parse(r#"{"type":"devices.watch"}"#).is_err());
        assert!(ServerToClient::parse("not json").is_err());
    }

    #[test]
    fn platform_serializes_as_contract_strings() {
        for (platform, expected) in [
            (Platform::Macos, "macos"),
            (Platform::Linux, "linux"),
            (Platform::Windows, "windows"),
        ] {
            let quoted = format!("\"{expected}\"");
            assert_eq!(serde_json::to_string(&platform).unwrap(), quoted);
            assert_eq!(serde_json::from_str::<Platform>(&quoted).unwrap(), platform);
        }
    }

    #[test]
    fn rest_bodies_match_contract() {
        let health = crate::HealthResponse::ok();
        assert_eq!(
            serde_json::to_string(&health).unwrap(),
            r#"{"status":"ok","version":"0.1.0"}"#
        );

        let devices = crate::DevicesResponse { devices: vec![] };
        assert_eq!(
            serde_json::to_string(&devices).unwrap(),
            r#"{"devices":[]}"#
        );
        assert_eq!(
            serde_json::from_str::<crate::DevicesResponse>(r#"{"devices":[]}"#).unwrap(),
            devices
        );
    }
}
