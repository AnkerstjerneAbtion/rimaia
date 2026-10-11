//! The binary, run as a process (task 046 Scope 8).

use std::process::Command;

#[test]
fn the_binary_refuses_to_start_without_its_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_rimaia-server"))
        .env_clear()
        .output()
        .expect("run the server binary");

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("RIMAIA_DATA_DIR"), "{stderr}");
}

#[test]
fn the_binary_names_a_bind_it_cannot_make() {
    let data = tempfile::Builder::new()
        .prefix("rimaia-server-binary-")
        .tempdir()
        .expect("a data directory");
    // Taken for the length of the test, so the binary's bind fails.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let address = taken.local_addr().expect("the bound address");

    // The inherited environment otherwise, so the platform's own variables
    // are there for the socket layer.
    let output = Command::new(env!("CARGO_BIN_EXE_rimaia-server"))
        .env("RIMAIA_DATA_DIR", data.path())
        .env("RIMAIA_LISTEN", address.to_string())
        .env("RIMAIA_PUBLIC_URL", "http://localhost:8787")
        .env("RIMAIA_GITHUB_CLIENT_ID", "Iv1.test")
        .env("RIMAIA_GITHUB_CLIENT_SECRET", "a-github-client-secret")
        .output()
        .expect("run the server binary");

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("could not listen on {address}")),
        "{stderr}"
    );
    assert!(
        data.path().join("rimaia.db").exists(),
        "the board is opened and migrated before the bind"
    );
    assert!(!stderr.contains("a-github-client-secret"), "{stderr}");
}

#[test]
fn the_binary_names_each_missing_sign_in_variable_and_never_the_secret() {
    let data = tempfile::Builder::new()
        .prefix("rimaia-server-binary-")
        .tempdir()
        .expect("a data directory");
    let full = [
        ("RIMAIA_PUBLIC_URL", "http://localhost:8787"),
        ("RIMAIA_GITHUB_CLIENT_ID", "Iv1.test"),
        ("RIMAIA_GITHUB_CLIENT_SECRET", "a-github-client-secret"),
    ];

    for (missing, _) in full {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rimaia-server"));
        command
            .env_clear()
            .env("RIMAIA_DATA_DIR", data.path())
            .env("RIMAIA_LISTEN", "127.0.0.1:0");
        for (name, value) in full.iter().filter(|(name, _)| *name != missing) {
            command.env(name, value);
        }
        let output = command.output().expect("run the server binary");

        assert!(!output.status.success(), "{missing}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("{missing} is not set")),
            "{stderr}"
        );
        assert!(!stderr.contains("a-github-client-secret"), "{stderr}");
    }
}

#[test]
fn the_binary_refuses_a_web_root_without_an_index_and_names_it() {
    let data = tempfile::Builder::new()
        .prefix("rimaia-server-binary-")
        .tempdir()
        .expect("a data directory");
    let empty = tempfile::Builder::new()
        .prefix("rimaia-server-web-root-")
        .tempdir()
        .expect("an empty web root");

    let output = Command::new(env!("CARGO_BIN_EXE_rimaia-server"))
        .env_clear()
        .env("RIMAIA_DATA_DIR", data.path())
        .env("RIMAIA_LISTEN", "127.0.0.1:0")
        .env("RIMAIA_PUBLIC_URL", "http://localhost:8787")
        .env("RIMAIA_GITHUB_CLIENT_ID", "Iv1.test")
        .env("RIMAIA_GITHUB_CLIENT_SECRET", "a-github-client-secret")
        .env("RIMAIA_WEB_ROOT", empty.path())
        .output()
        .expect("run the server binary");

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("RIMAIA_WEB_ROOT"), "{stderr}");
    assert!(
        stderr.contains(&empty.path().display().to_string()),
        "{stderr}"
    );
    assert!(
        !data.path().join("rimaia.db").exists(),
        "a misconfigured server opens nothing"
    );
}
