//! Release-side tool for the in-app updater (UPD-1). One implementation of the
//! signing format, shared with the app through the library.
//!
//!   update_sign keygen  --out KEYFILE             write PKCS#8 private key (0600); print public key
//!   update_sign pubkey  [--key KEYFILE]           print the public key for a private key
//!   update_sign manifest --version V --out FILE [--key KEYFILE] [--notes T] [--release-url U]
//!                        [--pause] --artifact PLATFORM=KIND=PATH=URL ...
//!   update_sign verify  --pubkey B64 --manifest FILE [--artifact PLATFORM=PATH ...]
//!   update_sign engine-pack --dir ONEDIR --out-prefix PREFIX [--part-size BYTES] [--level N]
//!                           [--threads N]        write PREFIX.tar.zst.NNN + PREFIX.pack.json
//!   update_sign engine-manifest --engine-version V --engine-api N --out FILE [--key KEYFILE]
//!                           --archive PLATFORM=VARIANT=PACKJSON=BASEURL ...
//!   update_sign engine-verify --pubkey B64 --manifest FILE [--archive PLATFORM=VARIANT=PACKJSON ...]
//!
//! Without --key the private key is read from TXTCASTER_UPDATE_SIGNING_KEY (base64
//! PKCS#8) so CI never writes it to disk. The key material is never printed.

use std::collections::{BTreeMap, HashMap};
use std::process::exit;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use sha2::{Digest, Sha256};
use txtcaster_lib::engine_install::manifest::{
    sign_engine_manifest, verify_and_parse_engine, EngineArchive, EngineManifest, EnginePart,
};
use txtcaster_lib::engine_install::pack::pack_engine;
use txtcaster_lib::updater::manifest::{verify_and_parse, Artifact, Envelope, Manifest};
use txtcaster_lib::updater::signing::{PublicKey, SigningKey};

fn die(msg: &str) -> ! {
    eprintln!("update_sign: {msg}");
    exit(2)
}

fn parse(args: &[String]) -> (HashMap<String, String>, Vec<String>, bool) {
    let (mut opts, mut artifacts, mut pause) = (HashMap::new(), Vec::new(), false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pause" => pause = true,
            "--artifact" | "--archive" => {
                artifacts.push(
                    args.get(i + 1)
                        .cloned()
                        .unwrap_or_else(|| die("--artifact needs a value")),
                );
                i += 1;
            }
            flag if flag.starts_with("--") => {
                let v = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| die(&format!("{flag} needs a value")));
                opts.insert(flag.trim_start_matches("--").to_string(), v);
                i += 1;
            }
            other => die(&format!("unexpected argument {other}")),
        }
        i += 1;
    }
    (opts, artifacts, pause)
}

fn load_key(opts: &HashMap<String, String>) -> SigningKey {
    let bytes = match opts.get("key") {
        Some(path) => std::fs::read(path).unwrap_or_else(|e| die(&format!("read {path}: {e}"))),
        None => B64
            .decode(
                std::env::var("TXTCASTER_UPDATE_SIGNING_KEY")
                    .unwrap_or_else(|_| die("no --key and TXTCASTER_UPDATE_SIGNING_KEY is unset"))
                    .trim(),
            )
            .unwrap_or_else(|_| die("TXTCASTER_UPDATE_SIGNING_KEY is not base64")),
    };
    SigningKey::from_pkcs8(&bytes).unwrap_or_else(|e| die(&e))
}

fn digest_file(path: &str) -> ([u8; 32], u64) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| die(&format!("read {path}: {e}")));
    (Sha256::digest(&bytes).into(), bytes.len() as u64)
}

fn digest_stream(path: &std::path::Path) -> ([u8; 32], u64) {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).unwrap_or_else(|e| die(&format!("read {}: {e}", path.display())));
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .unwrap_or_else(|e| die(&format!("read {}: {e}", path.display())));
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    (hasher.finalize().into(), total)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PackSummary {
    parts: Vec<String>,
    unpacked_size: u64,
}

fn read_summary(path: &str) -> (PackSummary, std::path::PathBuf) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| die(&format!("read {path}: {e}")));
    let summary: PackSummary =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let dir = std::path::Path::new(path)
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    (summary, dir)
}

fn parse_u64(opts: &HashMap<String, String>, name: &str, default: u64) -> u64 {
    opts.get(name).map_or(default, |v| {
        v.parse()
            .unwrap_or_else(|_| die(&format!("--{name} must be a whole number")))
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        die("usage: update_sign keygen|pubkey|manifest|verify ...")
    };
    let (opts, artifacts, pause) = parse(&args[1..]);
    match cmd.as_str() {
        "keygen" => {
            let out = opts.get("out").unwrap_or_else(|| die("--out required"));
            let (pkcs8, key) = SigningKey::generate().unwrap_or_else(|e| die(&e));
            std::fs::write(out, &pkcs8).unwrap_or_else(|e| die(&format!("write {out}: {e}")));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600));
            }
            println!("{}", key.public_key().to_base64());
        }
        "pubkey" => println!("{}", load_key(&opts).public_key().to_base64()),
        "manifest" => {
            let key = load_key(&opts);
            let version = opts
                .get("version")
                .unwrap_or_else(|| die("--version required"))
                .trim_start_matches('v')
                .to_string();
            semver::Version::parse(&version)
                .unwrap_or_else(|_| die(&format!("{version} is not semver")));
            let mut platforms = BTreeMap::new();
            for spec in &artifacts {
                let parts: Vec<&str> = spec.splitn(4, '=').collect();
                let [platform, kind, path, url] = parts[..] else {
                    die(&format!(
                        "--artifact wants PLATFORM=KIND=PATH=URL, got {spec}"
                    ))
                };
                let (digest, size) = digest_file(path);
                platforms.insert(
                    platform.to_string(),
                    Artifact {
                        url: url.to_string(),
                        sha256: hex::encode(digest),
                        size,
                        signature: key.sign_artifact_digest(&digest),
                        kind: Some(kind.to_string()),
                    },
                );
            }
            let manifest = Manifest {
                version,
                notes: opts.get("notes").cloned(),
                pub_date: Some(httpdate::fmt_http_date(std::time::SystemTime::now())),
                pause_updates: pause,
                release_url: opts.get("release-url").cloned(),
                platforms,
            };
            let payload = serde_json::to_vec(&manifest).unwrap();
            let envelope = Envelope {
                payload: B64.encode(&payload),
                signature: key.sign_manifest(&payload),
            };
            let out = opts.get("out").unwrap_or_else(|| die("--out required"));
            std::fs::write(out, serde_json::to_vec_pretty(&envelope).unwrap())
                .unwrap_or_else(|e| die(&format!("write {out}: {e}")));
            println!(
                "signed manifest for {} ({} artifact(s), pause={pause}) → {out}",
                manifest.version,
                manifest.platforms.len()
            );
        }
        "verify" => {
            let pubkey = PublicKey::from_base64(
                opts.get("pubkey")
                    .unwrap_or_else(|| die("--pubkey required")),
            )
            .unwrap_or_else(|e| die(&e));
            let path = opts
                .get("manifest")
                .unwrap_or_else(|| die("--manifest required"));
            let bytes = std::fs::read(path).unwrap_or_else(|e| die(&format!("read {path}: {e}")));
            let manifest =
                verify_and_parse(&bytes, &pubkey).unwrap_or_else(|e| die(&format!("REFUSED: {e}")));
            for spec in &artifacts {
                let (platform, file) = spec
                    .split_once('=')
                    .unwrap_or_else(|| die("--artifact wants PLATFORM=PATH"));
                let art = manifest
                    .platforms
                    .get(platform)
                    .unwrap_or_else(|| die(&format!("manifest has no {platform}")));
                let (digest, size) = digest_file(file);
                if hex::encode(digest) != art.sha256.to_lowercase() || size != art.size {
                    die(&format!("REFUSED: {platform} digest/size mismatch"));
                }
                pubkey
                    .verify_artifact_digest(&digest, &art.signature)
                    .unwrap_or_else(|e| die(&format!("REFUSED: {platform}: {e}")));
            }
            println!(
                "OK version={} pause={} platforms={:?}",
                manifest.version,
                manifest.pause_updates,
                manifest.platforms.keys().collect::<Vec<_>>()
            );
        }
        "engine-pack" => {
            let dir = opts.get("dir").unwrap_or_else(|| die("--dir required"));
            let prefix = opts
                .get("out-prefix")
                .unwrap_or_else(|| die("--out-prefix required"));
            let part_size = parse_u64(&opts, "part-size", 1900 * 1024 * 1024);
            let level = i32::try_from(parse_u64(&opts, "level", 10))
                .unwrap_or_else(|_| die("--level out of range"));
            let threads = u32::try_from(parse_u64(&opts, "threads", 0))
                .unwrap_or_else(|_| die("--threads out of range"));
            let threads = if threads == 0 {
                std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
            } else {
                threads
            };
            let packed = pack_engine(
                std::path::Path::new(dir),
                std::path::Path::new(prefix),
                part_size,
                level,
                threads,
            )
            .unwrap_or_else(|e| die(&format!("pack {dir}: {e}")));
            let summary = PackSummary {
                parts: packed
                    .parts
                    .iter()
                    .map(|p| {
                        p.path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| die("part has no file name"))
                    })
                    .collect(),
                unpacked_size: packed.unpacked_size,
            };
            let out = format!("{prefix}.pack.json");
            std::fs::write(&out, serde_json::to_vec_pretty(&summary).unwrap())
                .unwrap_or_else(|e| die(&format!("write {out}: {e}")));
            println!(
                "packed {dir}: {} part(s), {} bytes (unpacked {}), sha256 {} → {out}",
                packed.parts.len(),
                packed.size,
                packed.unpacked_size,
                hex::encode(packed.sha256)
            );
        }
        "engine-manifest" => {
            let key = load_key(&opts);
            let engine_version = opts
                .get("engine-version")
                .unwrap_or_else(|| die("--engine-version required"))
                .trim_start_matches('v')
                .to_string();
            semver::Version::parse(&engine_version)
                .unwrap_or_else(|_| die(&format!("{engine_version} is not semver")));
            let engine_api = u32::try_from(parse_u64(&opts, "engine-api", 0))
                .unwrap_or_else(|_| die("--engine-api out of range"));
            if engine_api == 0 {
                die("--engine-api required");
            }
            let mut platforms: BTreeMap<String, BTreeMap<String, EngineArchive>> = BTreeMap::new();
            for spec in &artifacts {
                let fields: Vec<&str> = spec.splitn(4, '=').collect();
                let [platform, variant, summary_path, base_url] = fields[..] else {
                    die(&format!(
                        "--archive wants PLATFORM=VARIANT=PACKJSON=BASEURL, got {spec}"
                    ))
                };
                let (summary, dir) = read_summary(summary_path);
                let mut whole = Sha256::new();
                let mut parts = Vec::new();
                let mut size = 0u64;
                for name in &summary.parts {
                    let path = dir.join(name);
                    let bytes_digest = digest_stream(&path);
                    let file = std::fs::File::open(&path)
                        .unwrap_or_else(|e| die(&format!("read {}: {e}", path.display())));
                    let mut reader = std::io::BufReader::new(file);
                    std::io::copy(&mut reader, &mut HashWriter(&mut whole))
                        .unwrap_or_else(|e| die(&format!("read {}: {e}", path.display())));
                    size += bytes_digest.1;
                    parts.push(EnginePart {
                        url: format!("{}/{name}", base_url.trim_end_matches('/')),
                        size: bytes_digest.1,
                        sha256: hex::encode(bytes_digest.0),
                    });
                }
                let digest: [u8; 32] = whole.finalize().into();
                platforms.entry(platform.to_string()).or_default().insert(
                    variant.to_string(),
                    EngineArchive {
                        size,
                        sha256: hex::encode(digest),
                        signature: key.sign_engine_archive_digest(&digest),
                        unpacked_size: summary.unpacked_size,
                        parts,
                    },
                );
            }
            if platforms.is_empty() {
                die("at least one --archive required");
            }
            let manifest = EngineManifest {
                engine_version,
                engine_api,
                platforms,
            };
            let envelope = sign_engine_manifest(&manifest, &key);
            let out = opts.get("out").unwrap_or_else(|| die("--out required"));
            std::fs::write(out, serde_json::to_vec_pretty(&envelope).unwrap())
                .unwrap_or_else(|e| die(&format!("write {out}: {e}")));
            println!(
                "signed engine manifest {} (api {}) → {out}",
                manifest.engine_version, manifest.engine_api
            );
        }
        "engine-verify" => {
            let pubkey = PublicKey::from_base64(
                opts.get("pubkey")
                    .unwrap_or_else(|| die("--pubkey required")),
            )
            .unwrap_or_else(|e| die(&e));
            let path = opts
                .get("manifest")
                .unwrap_or_else(|| die("--manifest required"));
            let bytes = std::fs::read(path).unwrap_or_else(|e| die(&format!("read {path}: {e}")));
            let manifest = verify_and_parse_engine(&bytes, &pubkey)
                .unwrap_or_else(|e| die(&format!("REFUSED: {e}")));
            for spec in &artifacts {
                let fields: Vec<&str> = spec.splitn(3, '=').collect();
                let [platform, variant, summary_path] = fields[..] else {
                    die("--archive wants PLATFORM=VARIANT=PACKJSON")
                };
                let archive = manifest
                    .archive(platform, variant)
                    .unwrap_or_else(|| die(&format!("manifest has no {platform}/{variant}")));
                let (summary, dir) = read_summary(summary_path);
                if summary.parts.len() != archive.parts.len() {
                    die(&format!("REFUSED: {platform}/{variant} part count differs"));
                }
                let mut whole = Sha256::new();
                for (name, part) in summary.parts.iter().zip(&archive.parts) {
                    let file = dir.join(name);
                    let (digest, size) = digest_stream(&file);
                    if hex::encode(digest) != part.sha256.to_lowercase() || size != part.size {
                        die(&format!("REFUSED: {name} digest/size mismatch"));
                    }
                    let reader = std::fs::File::open(&file)
                        .unwrap_or_else(|e| die(&format!("read {}: {e}", file.display())));
                    std::io::copy(
                        &mut std::io::BufReader::new(reader),
                        &mut HashWriter(&mut whole),
                    )
                    .unwrap_or_else(|e| die(&format!("read {}: {e}", file.display())));
                }
                let digest: [u8; 32] = whole.finalize().into();
                if hex::encode(digest) != archive.sha256.to_lowercase() {
                    die(&format!(
                        "REFUSED: {platform}/{variant} archive digest mismatch"
                    ));
                }
                pubkey
                    .verify_engine_archive_digest(&digest, &archive.signature)
                    .unwrap_or_else(|e| die(&format!("REFUSED: {platform}/{variant}: {e}")));
            }
            println!(
                "OK engine={} api={} platforms={:?}",
                manifest.engine_version,
                manifest.engine_api,
                manifest
                    .platforms
                    .iter()
                    .map(|(p, v)| format!("{p}:{:?}", v.keys().collect::<Vec<_>>()))
                    .collect::<Vec<_>>()
            );
        }
        other => die(&format!("unknown command {other}")),
    }
}

struct HashWriter<'a>(&'a mut Sha256);

impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
