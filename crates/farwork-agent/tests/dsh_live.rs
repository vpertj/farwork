//! Live read-only probe of a running DSH bridge.
//!
//! Ignored by default: it needs a DSH on this machine and must never run in CI or
//! touch a service the developer did not start. It exists so "the DSH runtime
//! really works" can be re-checked with this agent's own transport instead of a
//! one-off script, and it is read-only on purpose — `initialize`, `session.list`
//! and the catalogs only. Creating a session would run a real turn in somebody's
//! DSH, which is not something a test may do on its own.
//!
//! Run it with:
//!   cargo test -p farwork-agent --test dsh_live -- --ignored --nocapture

use farwork_agent::dsh::client::DshClient;
use farwork_agent::dsh::discovery;
use farwork_agent::dsh::handshake::InitializeParams;
use farwork_agent::dsh::rpc;

#[tokio::test]
#[ignore = "needs a running DSH bridge; read-only"]
async fn initialize_and_list_against_a_real_bridge() {
    let env = |key: &str| std::env::var(key).ok();
    let endpoint = match discovery::resolve(None, &env) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            println!("PROBE SKIP: no DSH bridge endpoint ({error})");
            return;
        }
    };
    println!(
        "PROBE endpoint {} pid={:?}",
        endpoint.address(),
        endpoint.pid
    );

    let connection = rpc::dial(&endpoint.address()).await.expect("dial");
    let client = DshClient::new(connection.client);
    let params = InitializeParams {
        auth_token: &endpoint.token,
        connector_id: "farwork-agent-probe",
        session_namespace: "farwork-probe",
        client_version: env!("CARGO_PKG_VERSION"),
    };
    let handshake = client
        .initialize(&endpoint.address(), &params)
        .await
        .expect("initialize");
    println!("PROBE identity {:?}", handshake.identity);
    println!("PROBE features {:?}", handshake.features);
    assert_eq!(handshake.identity.runtime, "dsh");
    assert!(
        handshake.features.events(),
        "the event stream must be offered"
    );
    assert_eq!(
        handshake.features.projection_version(),
        if handshake.features.projection_version == Some(2) {
            2
        } else {
            3
        }
    );

    let sessions = client.list_sessions(5).await.expect("session.list");
    println!("PROBE session.list count={}", sessions.len());
    for session in sessions.iter().take(5) {
        println!(
            "PROBE session {} external={:?} title={:?} cwd={:?}",
            session
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("-"),
            session.get("externalSessionId").and_then(|v| v.as_str()),
            session.get("title").and_then(|v| v.as_str()),
            session.get("cwd").and_then(|v| v.as_str()),
        );
    }

    let models = client.list_models().await.expect("catalog.listModels");
    println!("PROBE catalog.listModels count={}", models.len());
    for model in models.iter().take(3) {
        println!("PROBE model {} = {}", model.id, model.title);
    }
    let permissions = client
        .list_permissions()
        .await
        .expect("catalog.listPermissions");
    println!("PROBE catalog.listPermissions count={}", permissions.len());
    for permission in permissions.iter().take(4) {
        println!("PROBE permission {} = {}", permission.id, permission.title);
    }

    client.rpc().close().await;
}
