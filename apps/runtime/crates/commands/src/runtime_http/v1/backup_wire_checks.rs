use anyhow::{ensure, Context, Result};
use respire_app::snapshot::backup_archive::open_to_stage;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, io::Read, net::SocketAddr, path::Path, time::Duration};

const WAIT: Duration = Duration::from_secs(10);
const EXPORT: &str = "/v1/backups/export";

pub(super) fn verify_http(bound: SocketAddr, root: &Path, urk: [u8; 32], token: &str) -> Result<()> {
    let base = format!("http://{bound}");
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).timeout(WAIT).build();
    let get = |name: &str| agent.get(&format!("{base}{name}"));
    let post = |name: &str| agent.post(&format!("{base}{name}")).set("Authorization", &format!("Bearer {token}"));
    for (request, status) in [
        (get(EXPORT), 401),
        (get(EXPORT).set("Authorization", "Bearer stale-fixture"), 401),
        (get(EXPORT).set("Authorization", &format!("Bearer {token}")).set("Origin", "http://untrusted.invalid"), 403),
        (get(&format!("{EXPORT}?path=disallowed")).set("Authorization", &format!("Bearer {token}")), 400),
        (get(&format!("{EXPORT}?space_id=unknown-fixture")).set("Authorization", &format!("Bearer {token}")), 404),
    ] {
        match request.call() {
            Err(ureq::Error::Status(observed, _)) if observed == status => (),
            _ => anyhow::bail!("unexpected backup admission status; expected {status}"),
        }
    }
    assert!(backup_stage_count(root)? == 0);

    #[cfg(target_os = "linux")]
    {
        for (request, expected) in [
            (agent.post(&format!("{base}/v1/backups/import")), 401),
            (post(EXPORT), 405),
            (get("/v1/backups/import"), 405),
            (post("/api/runtime/private-backup/claim"), 401),
        ] {
            let error = request.call().expect_err("unexpected backup admission");
            ensure!(matches!(error, ureq::Error::Status(code, _) if code == expected),
                "unexpected backup authorization or method result: {error}");
        }
        ensure!(backup_stage_count(root)? == 0, "denied requests retained custody");
    }
    let save = |space: &str, content: &str| -> Result<String> {
        let response = post("/v1/memories").send_json(json!({"space_id": space, "content": content}))?;
        ensure!(response.status() == 201, "memory creation failed");
        let response: Value = response.into_json()?;
        Ok(response["id"].as_str().context("fixture memory ID absent")?.to_owned())
    };
    let parent = save("main", "isolated parent fixture memory")?;
    let created = post("/v1/spaces").send_json(json!({"name": "isolated backup fixture"}))?;
    ensure!(created.status() == 201, "device space creation failed");
    let create: Value = created.into_json()?;
    let space = create["id"].as_str().context("fixture device space ID absent")?;
    let child = save(space, "isolated child fixture memory")?;

    let keys = respire_app::SessionKeys::from_urk(urk)?;
    for (space, expected) in [("main", parent), (space, child)] {
        let response = get(&format!("{EXPORT}?space_id={space}"))
            .set("Authorization", &format!("Bearer {token}")).call()?;
        assert_eq!(response.status(), 200);
        assert_eq!(response.header("content-type"), Some("application/octet-stream"));
        assert_eq!(response.header("cache-control"), Some("no-store"));
        let mut wire = Vec::new();
        response.into_reader().take(8 * 1024 * 1024 + 1).read_to_end(&mut wire)?;
        ensure!(wire.len() <= 8 * 1024 * 1024, "fixture archive exceeded its byte budget");
        #[cfg(target_os = "linux")]
        {
            let upload = |payload: &[u8]| post("/v1/backups/import?space_id=main")
                .set("Content-Type", "application/octet-stream").send_bytes(payload);
            let reject = |payload: &[u8]| -> Result<()> {
                ensure!(matches!(upload(payload), Err(ureq::Error::Status(400, _))), "invalid upload accepted");
                ensure!(backup_stage_count(root)? == 0, "rejected upload reached confirmation worker");
                Ok(())
            };
            if space == "main" {
                reject(b"invalid envelope")?;
                reject(&wire[..wire.len()-1])?;
                let mut bad_seal = wire.clone();
                let field = br#""sealed_manifest_digest":""#;
                let seal = bad_seal.windows(field.len()).position(|bytes| bytes == field)
                    .context("fixture seal missing")? + field.len();
                bad_seal[seal] = if bad_seal[seal] == b'A' { b'B' } else { b'A' };
                let header_size = u32::from_be_bytes(wire[8..12].try_into()?) as usize;
                reject(&bad_seal[..12 + header_size])?;
                {
                    use std::{io::Write, net::{Shutdown, TcpStream}};
                    let mut stream = TcpStream::connect(bound)?;
                    stream.set_read_timeout(Some(WAIT))?;
                    stream.set_write_timeout(Some(WAIT))?;
                    write!(stream, "POST /v1/backups/import HTTP/1.1\r\nHost: {bound}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", wire.len())?;
                    stream.write_all(&wire[..wire.len()/2])?;
                    stream.shutdown(Shutdown::Write)?;
                    let mut reply = Vec::new();
                    stream.take(8192).read_to_end(&mut reply)?;
                    ensure!(!reply.starts_with(b"HTTP/1.1 202"), "aborted upload accepted");
                    ensure!(backup_stage_count(root)? == 0, "aborted upload reached worker");
                }
                let mut corrupt = wire.clone();
                *corrupt.last_mut().context("empty archive")? ^= 1;
                reject(&corrupt)?;
                let mut trailing = wire.clone();
                trailing.push(0);
                reject(&trailing)?;
                ensure!(wire.len() > 2 * 1024 * 1024, "fixture must cross the old buffer limit");
                // An unsized reader makes ureq send HTTP chunked framing. Split
                // even the magic across reads, then use odd-sized payload blocks.
                let accepted = post("/v1/backups/import?space_id=main")
                    .set("Content-Type", "application/octet-stream")
                    .send(SplitArchive { rest: &wire, first: true })?;
                assert_eq!(accepted.status(), 202);
                let receipt: Value = accepted.into_json()?;
                ensure!(matches!(upload(&wire), Err(ureq::Error::Status(503, _))),
                    "full pending custody admitted another upload");
                assert_eq!(backup_stage_count(root)?, 1);
                let id = receipt["operation_id"].as_str().context("missing operation")?;
                let uuid = uuid::Uuid::parse_str(id)?;
                assert_eq!(receipt, json!({"operation_id": id, "status": "pending"}));
                let status: Value = get(&format!("/v1/operations/{id}"))
                    .set("Authorization", &format!("Bearer {token}")).call()?.into_json()?;
                assert_eq!(status["state"], "pending");
                agent.delete(&format!("{base}/v1/operations/{uuid}"))
                    .set("Authorization", &format!("Bearer {token}")).call()?;
            } else {
                reject(&wire)?;
                let accepted = post(&format!("/v1/backups/import?space_id={space}"))
                    .set("Content-Type", "application/octet-stream").send_bytes(&wire)?;
                assert_eq!(accepted.status(), 202);
                let receipt: Value = accepted.into_json()?;
                let id = receipt["operation_id"].as_str().context("missing child operation")?;
                assert_eq!(backup_stage_count(root)?, 1);
                let uuid = uuid::Uuid::parse_str(id)?;
                agent.delete(&format!("{base}/v1/operations/{uuid}"))
                    .set("Authorization", &format!("Bearer {token}")).call()?;
                assert_eq!(backup_stage_count(root)?, 0);
            }
        }
        let output = tempfile::NamedTempFile::new_in(root)?;
        open_to_stage(&keys, "backup-fixture", wire.as_slice(), output.as_file(), || Ok(()))?;
        let db = rusqlite::Connection::open(output.path())?;
        let mut rows = db.prepare("SELECT id FROM memories WHERE deleted=0")?;
        let ids = rows.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<BTreeSet<String>>>()?;
        assert_eq!(ids, BTreeSet::from([expected]));
    }
    assert!(backup_stage_count(root)? == 0);
    Ok(())
}

fn backup_stage_count(root: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".backup-export-") || name.starts_with(".backup-archive-") {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(target_os = "linux")]
struct SplitArchive<'a> {
    rest: &'a [u8],
    first: bool,
}

#[cfg(target_os = "linux")]
impl Read for SplitArchive<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let limit = if std::mem::take(&mut self.first) { 3 } else { 8191 };
        let size = out.len().min(limit);
        self.rest.read(&mut out[..size])
    }
}
