use anyhow::Result;
use std::{
    path::Path,
    process::{Command, Stdio},
};

pub(super) fn command(
    candidate_root: Option<&Path>,
    fixture_executable: Option<&Path>,
) -> Result<Command> {
    let foreground = candidate_root.is_none();
    let executable = match fixture_executable {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe()?,
    };
    let mut command = Command::new(executable);
    // Transport credentials and account keys remain in the owning host. Only
    // OS loader paths and explicit local inference settings reach the child.
    command.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "SystemDrive",
        "USERPROFILE",
        "HOME",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "TMPDIR",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
    ] {
        if let Some(value) = std::env::var_os(name).filter(|_| {
            foreground
                || matches!(
                    name,
                    "PATH" | "SystemRoot" | "SystemDrive" | "LD_LIBRARY_PATH" | "DYLD_LIBRARY_PATH"
                )
        }) {
            command.env(name, value);
        }
    }
    for name in [
        "RSRS_DATA_DIR",
        "RSRS_M3_DIR",
        "RSRS_ENGINE",
        "RSRS_MODEL_VARIANT",
        "RSRS_ORT_PROFILE",
        #[cfg(feature = "cpu-owned-ort-experiment")]
        "RSRS_CPU_OWNED_ORT_EXPERIMENT",
        #[cfg(feature = "cpu-owned-ort-experiment")]
        "RSRS_CPU_OWNED_ORT_FILE",
        "RSRS_CORE_TEST_MODE",
        "RSRS_INTRA_THREADS",
    ] {
        if let Some(value) = crate::env::var_os(name).filter(|_| foreground) {
            command.env(name, value);
        }
    }
    if let Some(root) = candidate_root {
        command
            .current_dir(root)
            .env("RESPIRE_PRIVATE_CORE_CANDIDATE", "1");
        #[cfg(windows)]
        command.env("RESPIRE_CANDIDATE_PROVIDER_ROOT", crate::onnx::settings_dir());
        for name in ["RSRS_CORE_TEST_MODE", "RSRS_INTRA_THREADS"] {
            if let Some(value) = crate::env::var_os(name) {
                command.env(name, value);
            }
        }
        for name in [
            "RSRS_DATA_DIR",
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "TMPDIR",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
        ] {
            command.env(name, root);
        }
    }
    command
        .arg("--core-worker-internal")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    Ok(command)
}
