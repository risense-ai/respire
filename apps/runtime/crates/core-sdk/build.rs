use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

#[path = "host-winml-build.rs"]
mod host_winml_build;
#[path = "native-runtime-build.rs"]
mod native_runtime_build;

fn source_digest(root: &Path) -> Result<String, Box<dyn Error>> {
    fn add(root: &Path, path: &Path, hash: &mut Sha256) -> Result<(), Box<dyn Error>> {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() {
            let mut items = fs::read_dir(path)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<Result<Vec<_>, _>>()?;
            items.sort_unstable();
            for item in items {
                add(root, &item, hash)?;
            }
        } else if meta.is_file() {
            let name = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("invalid Core source filename")?
                .replace('\\', "/");
            let bytes = fs::read(path)?;
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        } else {
            return Err("Core source inputs must be regular files".into());
        }
        Ok(())
    }

    let mut hash = Sha256::new();
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "crates",
        "contracts/core-abi",
        "vendor/tokenizers",
    ] {
        let path = root.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        add(root, &path, &mut hash)?;
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn main() -> Result<(), Box<dyn Error>> {
    host_winml_build::prepare()?;
    println!("cargo:rerun-if-changed=native-runtime-build.rs");
    println!("cargo:rustc-check-cfg=cfg(model_input_stream_contract)");
    if env::var_os("CARGO_FEATURE_MODEL_INPUT_STREAM_EXPERIMENT").is_some() {
        println!("cargo:rustc-cfg=model_input_stream_contract");
    }
    let target = env::var("TARGET")?;
    let features = env::var("CARGO_CFG_TARGET_FEATURE")?;
    let static_crt = features.split(',').any(|feature| feature == "crt-static");
    if env::var("CARGO_CFG_PANIC")? != "unwind" {
        return Err("the Core ABI requires a panic=unwind host".into());
    }
    if target.contains("windows-msvc") && static_crt {
        return Err("Windows Core requires the dynamic CRT (-crt-static)".into());
    }
    if target.contains("musl") && !static_crt {
        return Err("musl Core requires the static CRT (+crt-static)".into());
    }

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    let runtime_lock = root.join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", runtime_lock.display());
    let source = source_digest(&root.join("../../../core"))?;
    let (files, archive) = native_runtime_build::prepare(&target, &out)?;
    let manifest = serde_json::to_vec(&json!({
        "mode": "in_repository_source",
        "target": target,
        "core_source_sha256": source,
        "runtime_cargo_lock_sha256": format!("{:x}", Sha256::digest(fs::read(runtime_lock)?)),
        "ort_public_archive_sha256": archive,
        "runtime_files": files,
    }))?;
    fs::write(out.join("source-build.json"), &manifest)?;
    println!(
        "cargo:rustc-env=RESPIRE_CORE_SOURCE_MANIFEST_SHA256={:x}",
        Sha256::digest(manifest)
    );
    println!("cargo:rustc-env=RESPIRE_CORE_BUILD_TARGET={target}");
    let files = files.into_iter().collect::<Vec<_>>();
    fs::write(out.join("runtime_files.rs"), format!("&{files:?}"))?;
    if target.ends_with("-linux-gnu") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    } else if target.contains("apple-darwin") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    }
    Ok(())
}
