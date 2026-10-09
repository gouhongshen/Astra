//! Native authority selection and legacy compatibility at the binary boundary.
use std::process::Command;

#[tokio::test]
#[cfg(unix)]
async fn status_distinguishes_pending_owner_without_refresh_or_credential_disclosure() {
    use astra_credentials::native::{Environment, NativeSession, NativeStore};
    use fs2::FileExt;
    use std::os::unix::fs::OpenOptionsExt;
    let root = tempfile::tempdir().unwrap();
    let store = NativeStore::with_directory(root.path().join(".moi"));
    let issuer = "https://uc.example.test/realms/moi";
    let (session, _) = store
        .publish(NativeSession {
            environment: Environment {
                issuer: issuer.into(),
                astra_url: "https://astra.example.test".into(),
                moi_url: "https://moi.example.test".into(),
                authorization_endpoint: format!("{issuer}/protocol/openid-connect/auth"),
                token_endpoint: format!("{issuer}/protocol/openid-connect/token"),
                revocation_endpoint: format!("{issuer}/protocol/openid-connect/revoke"),
                jwks_uri: format!("{issuer}/protocol/openid-connect/certs"),
            },
            generation: String::new(),
            subject: "user".into(),
            session_id: "session".into(),
            astra_user_id: "astra-user".into(),
            moi_principal_id: "moi-user".into(),
            catalog_user_id: "catalog-user".into(),
            access_token: "private-access".into(),
            refresh_token: "private-refresh".into(),
            expires_at: 0,
            workspace_id: None,
            role_id: None,
            refresh_pending: false,
        })
        .unwrap();
    let path = root.path().join(".moi/auth.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["sessions"][session.environment.key()]["refresh_pending"] = true.into();
    let original = serde_json::to_vec(&state).unwrap();
    std::fs::write(&path, &original).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(
            root.path()
                .join(format!(".moi/refresh-{}.lock", session.environment.key())),
        )
        .unwrap();
    lock.lock_exclusive().unwrap();
    for expected in ["refresh_in_progress", "reauthentication_required"] {
        if expected == "reauthentication_required" {
            FileExt::unlock(&lock).unwrap();
        }
        let output =
            client_output(isolated_client(root.path()).args(["auth", "status", "--json"])).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status["state"], expected);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let public = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!public.contains("private-access") && !public.contains("private-refresh"));
    }
}

#[cfg(unix)]
fn isolated_client(root: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_astra"));
    command
        .env_clear()
        .env("HOME", root)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

#[cfg(unix)]
async fn client_output(command: &mut tokio::process::Command) -> std::process::Output {
    tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
        .await
        .expect("local login must complete without browser/interactive input")
        .unwrap()
}

#[tokio::test]
#[cfg(unix)]
async fn unused_moi_directory_does_not_block_legacy_login() {
    use std::os::unix::fs::PermissionsExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join(".moi");
    std::fs::create_dir(&auth).unwrap();
    std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token":"synthetic-access", "refresh_token":"synthetic-refresh", "user_id":"local-user"
        })))
        .expect(1)
        .mount(&server).await;
    let output = client_output(
        isolated_client(root.path())
            .env("ASTRA_API_URL", server.uri())
            .args([
                "login",
                "--username",
                "local-user",
                "--password",
                "synthetic-password",
            ]),
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_dir(&auth).unwrap().count(), 0);
    assert_eq!(
        std::fs::metadata(&auth).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[tokio::test]
#[cfg(unix)]
async fn fresh_process_login_cannot_downgrade_a_selected_uc_environment() {
    use astra_credentials::native::{Environment, NativeSession, NativeStore};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    for (status, body) in [
        (404, json!({})),
        (200, json!({"password":true, "uc":null, "memoria":null})),
        (
            200,
            json!({"password":true, "memoria":{
                "issuer":"https://memory.example.test", "authorization_url":"https://memory.example.test"
            }}),
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/auth/methods"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body.clone()))
            .mount(&server)
            .await;
        for logged_out in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = NativeStore::with_directory(root.path().join(".moi"));
            let issuer = "https://uc.example.test/realms/moi";
            store
                .publish(NativeSession {
                    environment: Environment {
                        issuer: issuer.into(),
                        astra_url: server.uri(),
                        moi_url: "https://moi.example.test/newmoi".into(),
                        authorization_endpoint: format!("{issuer}/protocol/openid-connect/auth"),
                        token_endpoint: format!("{issuer}/protocol/openid-connect/token"),
                        revocation_endpoint: format!("{issuer}/protocol/openid-connect/revoke"),
                        jwks_uri: format!("{issuer}/protocol/openid-connect/certs"),
                    },
                    generation: String::new(),
                    subject: "user".into(),
                    session_id: "session".into(),
                    astra_user_id: "astra-user".into(),
                    moi_principal_id: "moi-user".into(),
                    catalog_user_id: "catalog-user".into(),
                    access_token: "synthetic-access".into(),
                    refresh_token: "synthetic-refresh".into(),
                    expires_at: 0,
                    workspace_id: None,
                    role_id: None,
                    refresh_pending: false,
                })
                .unwrap();
            if logged_out {
                store.logout().unwrap();
            }
            let auth_path = root.path().join(".moi/auth.json");
            let before = std::fs::read(&auth_path).unwrap();
            let output = client_output(isolated_client(root.path()).arg("login")).await;
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success());
            assert!(
                error.contains("requires UC login"),
                "status={status}, logged_out={logged_out}: {error}"
            );
            assert!(!String::from_utf8_lossy(&output.stdout).contains("Username"));
            assert_eq!(before, std::fs::read(&auth_path).unwrap());

            // The explicit legacy escape hatch still reaches password input,
            // without consuming or modifying the selected MOI session.
            if body.get("memoria").is_none_or(serde_json::Value::is_null) {
                let output = client_output(
                    isolated_client(root.path())
                        .env("ASTRA_API_URL", server.uri())
                        .args(["--profile", "personal", "login"]),
                )
                .await;
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("Username cannot be empty")
                );
                assert_eq!(before, std::fs::read(&auth_path).unwrap());
            }
        }
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.url.path() == "/auth/methods")
        );
    }
}

#[test]
fn normal_commands_reject_workspace_auth_directory_but_keep_shell_override() {
    let root = tempfile::tempdir().unwrap();
    let trusted = root.path().join("trusted");
    let untrusted = root.path().join("untrusted");
    std::fs::write(
        root.path().join(".env"),
        format!("MOI_AUTH_DIR={}\n", untrusted.display()),
    )
    .unwrap();
    for shell_override in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_astra"));
        command
            .env_clear()
            .env("HOME", root.path())
            .current_dir(root.path());
        // An invalid local setting makes this deterministic and offline even
        // when the trusted shell override passes the dotenv boundary.
        command.args(["--settings", "{invalid"]);
        if shell_override {
            command.env("MOI_AUTH_DIR", &trusted);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            diagnostic.contains("MOI_AUTH_DIR"),
            !shell_override,
            "{diagnostic}"
        );
    }
    assert!(!untrusted.exists(), "workspace auth store was accessed");
    let output = Command::new(env!("CARGO_BIN_EXE_astra"))
        .env_clear()
        .env("HOME", root.path())
        .env("MOI_AUTH_DIR", &trusted)
        .current_dir(root.path())
        .args(["auth", "status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("not_configured"));
    assert!(!untrusted.exists());
}
