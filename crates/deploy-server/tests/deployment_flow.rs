//! End-to-end deployment tests against a real `deploy-server` process.
//!
//! These are the Rust replacement for the old tool's
//! `src/__tests__/serverHandlers.test.ts` and `test/deployment.test.ts` /
//! `test/multipartUpload.test.ts` / `test/rollback.test.ts`. They run over the
//! JSON-RPC transport with authorization switched on, so every assertion here
//! also proves the call was authorized rather than waved through.

mod common;

use serde_json::json;

use common::*;

/// A permissive laptop key, as `deploy:**` would be issued in auth-center.
const KEY: &str = "laptop-key";

fn start(root: &TempRoot) -> (StubAuthCenter, DeployServer) {
    let stub = start_stub_auth_center(granting(vec![grant(KEY, &["deploy:**"])]));
    let server = DeployServer::start(
        root,
        ServerOptions {
            auth_url: Some(stub.base_url.clone()),
            ..ServerOptions::default()
        },
    );
    (stub, server)
}

// ---------------------------------------------------------------------------
// 1. A full round trip
// ---------------------------------------------------------------------------

#[test]
fn full_round_trip_uploads_single_shot_and_multipart_files() {
    let root = TempRoot::new("round-trip");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);

    create_project(&rpc, "basic-app", "basic-staging");

    // The 90KB file's base64 is well past the client's 80KB single-request
    // ceiling, so it has to go up in parts.
    let big = large_content(90 * 1024);
    let files = vec![
        file("package.json", r#"{"name":"basic-app","version":"1.0.0"}"#),
        file("index.js", "console.log('Hello from Basic App v1!');\n"),
        file("public/index.html", "<h1>Welcome to Basic App</h1>\n"),
        binary_file("assets/bundle.js", big.clone()),
    ];
    let config = config_for("basic-app", true, "");

    let deploy_name = create_deployment(&rpc, "basic-app", &config, &files);
    assert!(deploy_name.starts_with("basic-app-"));

    // Nothing is on the server yet, so every file is needed.
    let needed = upload_needed(&rpc, &deploy_name, &files);
    assert_eq!(
        needed.len(),
        4,
        "every file should be requested: {needed:?}"
    );
    assert!(needed.contains(&"assets/bundle.js".to_string()));

    finish_and_activate(&rpc, &deploy_name);

    let dir = server.project_dir("basic-app");
    assert_eq!(
        read_file(&dir.join("package.json")),
        r#"{"name":"basic-app","version":"1.0.0"}"#
    );
    assert!(read_file(&dir.join("index.js")).contains("Hello from Basic App v1!"));
    assert!(read_file(&dir.join("public/index.html")).contains("Welcome to Basic App"));

    // The multipart file has to be reassembled byte for byte, in offset order.
    let uploaded = std::fs::read(dir.join("assets/bundle.js")).unwrap();
    assert_eq!(uploaded.len(), big.len());
    assert_eq!(uploaded, big);

    let listed = rpc.ok("listDeployments", json!({ "projectName": "basic-app" }));
    assert_eq!(listed["activeDeployName"], deploy_name);
    assert_eq!(listed["deployments"][0]["is_active"], true);
    // R7: history can answer "who shipped this".
    assert_eq!(
        listed["deployments"][0]["authorized_by_key_id"],
        format!("key_{KEY}")
    );

    // A second deploy of identical content asks for nothing: the server already
    // has every hash.
    let deploy_two = create_deployment(&rpc, "basic-app", &config, &files);
    let needed = upload_needed(&rpc, &deploy_two, &files);
    assert!(needed.is_empty(), "content dedup should ask for nothing");
    finish_and_activate(&rpc, &deploy_two);
}

#[test]
fn verification_fails_in_band_when_a_file_never_arrived() {
    let root = TempRoot::new("verify-missing");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "partial-app", "partial-staging");

    let files = vec![file("a.txt", "a"), file("b.txt", "b")];
    let deploy_name = create_deployment(
        &rpc,
        "partial-app",
        &config_for("partial-app", true, ""),
        &files,
    );
    rpc.ok("getNeededFiles", json!({ "deployName": deploy_name }));
    upload(&rpc, &deploy_name, &files[0]);

    let verified = rpc.ok("verifyDeployment", json!({ "deployName": deploy_name }));
    assert_eq!(verified["status"], "error");
    assert!(verified["error"]
        .as_str()
        .unwrap()
        .contains("1 files are missing"));
}

// ---------------------------------------------------------------------------
// 2. The batched-manifest path
// ---------------------------------------------------------------------------

#[test]
fn a_manifest_too_large_to_inline_goes_up_in_batches() {
    let root = TempRoot::new("batched-manifest");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "big-app", "big-staging");

    // Past the client's 500-entry inline limit, so the CLI would use
    // addManifestFiles; the batches are sent here the same way.
    const FILE_COUNT: usize = 550;
    const BATCH_SIZE: usize = 500;

    let files: Vec<SourceFile> = (0..FILE_COUNT)
        .map(|i| {
            file(
                &format!("src/mod-{i:04}.js"),
                &format!("export const n = {i};\n"),
            )
        })
        .collect();
    let config = config_for("big-app", true, "");

    // createDeployment carries an empty manifest in this flow.
    let created = rpc.ok(
        "createDeployment",
        json!({
            "projectName": "big-app",
            "sourceFileManifest": [],
            "sourceFileConfig": config,
        }),
    );
    let deploy_name = created["deployName"].as_str().unwrap().to_string();

    for batch in files.chunks(BATCH_SIZE) {
        rpc.ok(
            "addManifestFiles",
            json!({ "deployName": deploy_name, "files": manifest_of(batch) }),
        );
    }
    rpc.ok("finalizeManifest", json!({ "deployName": deploy_name }));

    let needed = upload_needed(&rpc, &deploy_name, &files);
    assert_eq!(
        needed.len(),
        FILE_COUNT,
        "the finalized manifest should cover every batch"
    );

    finish_and_activate(&rpc, &deploy_name);

    let dir = server.project_dir("big-app");
    assert_eq!(
        read_file(&dir.join("src/mod-0000.js")),
        "export const n = 0;\n"
    );
    assert_eq!(
        read_file(&dir.join("src/mod-0549.js")),
        "export const n = 549;\n"
    );
}

// ---------------------------------------------------------------------------
// 3. Directory layout and rollback
// ---------------------------------------------------------------------------

#[test]
fn update_in_place_deploys_share_one_directory() {
    let root = TempRoot::new("in-place");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "inplace-app", "inplace-staging");

    let config = config_for("inplace-app", true, "");

    let first = deploy(&rpc, "inplace-app", &config, &[file("app.js", "v1\n")]);
    let second = deploy(&rpc, "inplace-app", &config, &[file("app.js", "v2\n")]);
    assert_ne!(first, second);

    let dir = server.project_dir("inplace-app");
    assert_eq!(read_file(&dir.join("app.js")), "v2\n");
    // No per-deployment directory was created alongside it.
    assert!(!server.deploys_dir.join(&first).exists());
    assert!(!server.deploys_dir.join(&second).exists());
}

#[test]
fn versioned_deploys_get_their_own_directory_and_rollback_repoints() {
    let root = TempRoot::new("versioned");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "versioned-app", "versioned-staging");

    let config = config_for("versioned-app", false, "");

    let first = deploy(&rpc, "versioned-app", &config, &[file("app.js", "v1\n")]);
    let second = deploy(&rpc, "versioned-app", &config, &[file("app.js", "v2\n")]);

    let first_dir = server.deploys_dir.join(&first);
    let second_dir = server.deploys_dir.join(&second);
    assert_eq!(read_file(&first_dir.join("app.js")), "v1\n");
    assert_eq!(read_file(&second_dir.join("app.js")), "v2\n");
    // A versioned deploy never writes into a directory named for the project.
    assert!(!server.project_dir("versioned-app").exists());

    let listed = rpc.ok("listDeployments", json!({ "projectName": "versioned-app" }));
    assert_eq!(listed["activeDeployName"], second);

    rpc.ok(
        "rollback",
        json!({ "projectName": "versioned-app", "deployName": first }),
    );

    let listed = rpc.ok("listDeployments", json!({ "projectName": "versioned-app" }));
    assert_eq!(listed["activeDeployName"], first);

    // Rollback only repoints; both directories are left intact so it can be
    // rolled forward again.
    assert_eq!(read_file(&first_dir.join("app.js")), "v1\n");
    assert_eq!(read_file(&second_dir.join("app.js")), "v2\n");
}

#[test]
fn rollback_refuses_a_deployment_belonging_to_another_project() {
    let root = TempRoot::new("rollback-cross");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "project-a", "shared-resource");
    create_project(&rpc, "project-b", "shared-resource");

    let config_a = config_for("project-a", false, "");
    let a_deploy = deploy(&rpc, "project-a", &config_a, &[file("a.txt", "a\n")]);

    let error = rpc
        .call(
            "rollback",
            json!({ "projectName": "project-b", "deployName": a_deploy }),
        )
        .unwrap_err();
    assert!(
        matches!(&error, RpcError::Method(message) if message.contains("not found for project")),
        "got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Leftover deletion, and the file that must survive it
// ---------------------------------------------------------------------------

#[test]
fn a_file_absent_from_the_new_manifest_is_deleted() {
    let root = TempRoot::new("leftovers");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "leftover-app", "leftover-staging");

    let config = config_for("leftover-app", true, "");
    deploy(
        &rpc,
        "leftover-app",
        &config,
        &[file("keep.js", "keep\n"), file("drop.js", "drop\n")],
    );

    let dir = server.project_dir("leftover-app");
    assert!(dir.join("drop.js").exists());

    // The second deploy no longer ships drop.js.
    deploy(&rpc, "leftover-app", &config, &[file("keep.js", "keep\n")]);

    assert!(dir.join("keep.js").exists());
    assert!(
        !dir.join("drop.js").exists(),
        "a file no longer in the manifest should be deleted"
    );
}

/// The 2026-05-23 production-database-wipe regression.
///
/// hotlaps' `api-staging` config was missing `ignore backend/data`, so an
/// update-in-place deploy saw the live production SQLite database as an
/// orphaned file and deleted it. The `ignore` rule is the only thing standing
/// between a server-side file and `finishUploads`, so this asserts it end to
/// end: the database survives, and an ordinary leftover in the same deploy
/// still gets removed — proving the deletion pass ran at all rather than
/// silently doing nothing.
#[test]
fn an_ignored_destination_file_survives_a_deploy_2026_05_23_database_wipe_regression() {
    let root = TempRoot::new("ignore-regression");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "hotlaps-api", "hotlaps-staging");

    let config = config_for("hotlaps-api", true, "ignore backend/data\n");

    deploy(&rpc, "hotlaps-api", &config, &[file("server.js", "v1\n")]);

    // Server-side state that the deploy does not ship: a live database under an
    // ignored directory, and an ordinary stray file that is not ignored.
    let dir = server.project_dir("hotlaps-api");
    let production_db = dir.join("backend/data/production.sqlite");
    write_file(&production_db, b"REAL PRODUCTION DATA");
    let stray = dir.join("stray.log");
    write_file(&stray, b"not in any manifest");

    deploy(&rpc, "hotlaps-api", &config, &[file("server.js", "v2\n")]);

    assert_eq!(
        read_file(&production_db),
        "REAL PRODUCTION DATA",
        "a file under an `ignore` rule must never be treated as a leftover"
    );
    assert!(
        !stray.exists(),
        "the leftover deletion pass must still have run"
    );
    assert_eq!(read_file(&dir.join("server.js")), "v2\n");
}

#[test]
fn preview_reports_the_same_deletions_finish_uploads_would_make() {
    let root = TempRoot::new("preview");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "preview-app", "preview-staging");

    let config = config_for("preview-app", true, "ignore data\n");
    deploy(
        &rpc,
        "preview-app",
        &config,
        &[file("keep.js", "keep\n"), file("drop.js", "drop\n")],
    );

    let dir = server.project_dir("preview-app");
    write_file(&dir.join("data/db.sqlite"), b"live");

    let next = vec![file("keep.js", "changed\n")];
    let preview = rpc.ok(
        "previewDeployment",
        json!({
            "projectName": "preview-app",
            "sourceFileManifest": manifest_of(&next),
            "sourceFileConfig": config,
        }),
    );

    let to_upload: Vec<&str> = preview["filesToUpload"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["relPath"].as_str().unwrap())
        .collect();
    assert_eq!(to_upload, vec!["keep.js"]);

    let to_delete: Vec<&str> = preview["filesToDelete"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    assert_eq!(
        to_delete,
        vec!["drop.js"],
        "the ignored database is not listed"
    );
}

// ---------------------------------------------------------------------------
// 5. preserve-existing-files
// ---------------------------------------------------------------------------

#[test]
fn preserved_files_survive_a_redeploy_and_are_collected_once_stale() {
    let root = TempRoot::new("preserve");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "preserve-app", "preserve-staging");

    let config = config_for(
        "preserve-app",
        true,
        "preserve-existing-files static/**\npreserve-existing-files-max-age 1h\n",
    );

    deploy(
        &rpc,
        "preserve-app",
        &config,
        &[file("index.html", "<h1>v1</h1>\n")],
    );

    // Content-hashed assets from an earlier build: not in any manifest, but
    // still referenced by pages the browser has cached.
    let dir = server.project_dir("preserve-app");
    let fresh = dir.join("static/app.fresh.js");
    let stale = dir.join("static/app.stale.js");
    write_file(&fresh, b"fresh asset");
    write_file(&stale, b"stale asset");
    set_mtime_hours_ago(&stale, 3);

    deploy(
        &rpc,
        "preserve-app",
        &config,
        &[file("index.html", "<h1>v2</h1>\n")],
    );

    assert_eq!(
        read_file(&fresh),
        "fresh asset",
        "a preserved file inside the max age must survive the redeploy"
    );
    assert!(
        !stale.exists(),
        "a preserved file past preserve-existing-files-max-age should be collected"
    );
}

#[test]
fn without_a_max_age_preserved_files_are_never_collected() {
    let root = TempRoot::new("preserve-forever");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "forever-app", "forever-staging");

    let config = config_for("forever-app", true, "preserve-existing-files static/**\n");
    deploy(&rpc, "forever-app", &config, &[file("index.html", "v1\n")]);

    let ancient = server.project_dir("forever-app").join("static/old.js");
    write_file(&ancient, b"ancient");
    set_mtime_hours_ago(&ancient, 24 * 400);

    deploy(&rpc, "forever-app", &config, &[file("index.html", "v2\n")]);

    assert_eq!(read_file(&ancient), "ancient");
}

// ---------------------------------------------------------------------------
// Path traversal, over the wire
// ---------------------------------------------------------------------------

#[test]
fn an_upload_cannot_escape_its_deployment_directory() {
    let root = TempRoot::new("traversal");
    let (_stub, server) = start(&root);
    let rpc = server.client(KEY);
    create_project(&rpc, "guarded-app", "guarded-staging");

    let files = vec![file("app.js", "ok\n")];
    let deploy_name = create_deployment(
        &rpc,
        "guarded-app",
        &config_for("guarded-app", true, ""),
        &files,
    );

    let error = rpc
        .call(
            "uploadOneFile",
            json!({
                "deployName": deploy_name,
                "relPath": "../../escaped.txt",
                "contentBase64": base64_of(b"nope"),
            }),
        )
        .unwrap_err();
    assert!(
        matches!(&error, RpcError::Method(message) if message.contains("Invalid path")),
        "got {error:?}"
    );
    assert!(!server.deploys_dir.join("escaped.txt").exists());
    assert!(!root.join("escaped.txt").exists());
}
