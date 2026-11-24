use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

// Exact selected public distribution keys from ort-sys 2.0.0-rc.13's dist.tsv.
// The Cargo dependency verifies these archive digests before extraction.
const AUTO_DISTRIBUTIONS: &[(&str, &str)] = &[
    (
        "x86_64-pc-windows-msvc",
        "f7c654b3729cb9e5ad2a36a0c38e5b48e63bf4eed22968931aed33a0ad0b527d",
    ),
    (
        "aarch64-pc-windows-msvc",
        "56df72970baeea08295db33f15f985cd74f4d4cd00e0099c44aba1504c5d140e",
    ),
    (
        "x86_64-unknown-linux-gnu",
        "e454f710f8a49f53aa5b4ff51e3454ae1835777e431c6c35c5255ce6f205fd68",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "06a050ab9137ccb32421d0cb49e9ccf72d9e18ab0aeb8f8d038d1b5cc844b35a",
    ),
    (
        "aarch64-apple-darwin",
        "6934874e2e953576d9c1db47ff1af39c62c4f4220dbe6f988e131f72879674c7",
    ),
];

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn input(target: &str) -> Result<(PathBuf, Option<&'static str>)> {
    if let Some(explicit) = env::var_os("ORT_LIB_PATH").or_else(|| env::var_os("ORT_LIB_LOCATION"))
    {
        let root = PathBuf::from(explicit);
        // Requiring absolute paths prevents two build scripts with different cwd
        // values from linking and inspecting different native installations.
        if !root.is_absolute() {
            return Err("ORT_LIB_PATH/ORT_LIB_LOCATION must be an absolute path".into());
        }
        return Ok((root, None));
    }
    let digest = AUTO_DISTRIBUTIONS
        .iter()
        .find_map(|(triple, hash)| (target == *triple).then_some(*hash))
        .ok_or("provide the supported native ONNX Runtime with ORT_LIB_PATH for this target")?;
    let cached = env::var_os("ORT_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(target_os = "linux") {
                if let Some(xdg) = env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                {
                    return Some(xdg);
                }
            }
            dirs::cache_dir().map(|path| path.join("ort.pyke.io"))
        })
        .ok_or("cannot locate the ONNX Runtime supplier cache; set ORT_LIB_PATH")?;
    Ok((cached.join("dfbin").join(target).join(digest), Some(digest)))
}

fn is_runtime_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "directml.dll"
        || n == "onnxruntime.dll"
        || n == "libonnxruntime.dylib"
        || n == "libonnxruntime.so"
        || n.starts_with("libonnxruntime.so.")
        || (n.starts_with("onnxruntime_providers_") && n.ends_with(".dll"))
        || (n.starts_with("libonnxruntime_providers_")
            && (n.ends_with(".so") || n.ends_with(".dylib")))
}

fn stage(destination: &Path, source: &[u8], expected: &str) -> Result<()> {
    if fs::symlink_metadata(destination).is_ok_and(|metadata| metadata.file_type().is_file())
        && fs::read(destination).is_ok_and(|bytes| digest(&bytes) == expected)
    {
        return Ok(());
    }
    let name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid runtime resource filename")?;
    let temporary = destination.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    fs::write(&temporary, source)?;
    if let Err(error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

pub fn prepare(
    target: &str,
    out: &Path,
) -> Result<(BTreeMap<String, String>, Option<&'static str>)> {
    for name in [
        "ORT_LIB_PATH",
        "ORT_LIB_LOCATION",
        "ORT_CACHE_DIR",
        "XDG_CACHE_HOME",
        "ORT_PREFER_DYNAMIC_LINK",
        "MLX_RS_METAL_PATH",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let (root, archive) = input(target)?;
    let dynamic = env::var("ORT_PREFER_DYNAMIC_LINK")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(&root)
        .map_err(|error| format!("read ONNX Runtime input {}: {error}", root.display()))?
    {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "non-UTF-8 ONNX Runtime input filename")?;
        if is_runtime_file(&name) {
            files.insert(name, entry.path());
        }
    }
    if target.contains("windows-msvc") && !files.contains_key("DirectML.dll") {
        return Err("DirectML.dll is missing from the linked ONNX Runtime input".into());
    }
    if dynamic
        && !files.keys().any(|name| {
            name == "onnxruntime.dll"
                || name == "libonnxruntime.dylib"
                || name.starts_with("libonnxruntime.so")
        })
    {
        return Err(
            "dynamic ONNX Runtime was selected but its linked runtime library is absent".into(),
        );
    }
    if env::var_os("CARGO_FEATURE_MLX").is_some() && target == "aarch64-apple-darwin" {
        let metal = PathBuf::from(
            env::var_os("MLX_RS_METAL_PATH")
                .ok_or("MLX source builds require an absolute MLX_RS_METAL_PATH")?,
        );
        if !metal.is_absolute() {
            return Err("MLX_RS_METAL_PATH must be absolute".into());
        }
        files.insert("mlx.metallib".to_owned(), metal.join("mlx.metallib"));
    }
    let profile = out
        .ancestors()
        .nth(3)
        .ok_or("unexpected Cargo output directory")?;
    let directories = [
        profile.to_owned(),
        profile.join("deps"),
        profile.join("examples"),
    ];
    for directory in &directories {
        fs::create_dir_all(directory)?;
    }
    let mut pins = BTreeMap::new();
    for (name, input) in files {
        println!("cargo:rerun-if-changed={}", input.display());
        let bytes = fs::read(&input)?;
        let checksum = digest(&bytes);
        for directory in &directories {
            stage(&directory.join(&name), &bytes, &checksum)?;
        }
        pins.insert(name, checksum);
    }
    Ok((pins, archive))
}
