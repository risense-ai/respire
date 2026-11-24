/// Released runtime build metadata consumed by the CLI downloader and client2.
#[derive(Clone, Debug, Deserialize)]
pub struct BuildMetadata {
    pub schema_version: u64,
    pub runtime_version: String,
    pub tag: String,
    #[serde(default, deserialize_with = "deserialize_metadata_string")]
    pub triple: Option<String>,
    #[serde(default, deserialize_with = "deserialize_metadata_string")]
    pub target_triple: Option<String>,
    #[serde(default, deserialize_with = "deserialize_metadata_string")]
    pub archive_file: Option<String>,
    /// Legacy archive checksum field; it must agree with `archive_sha256`.
    #[serde(default, deserialize_with = "deserialize_metadata_string")]
    pub sha256: Option<String>,
    #[serde(default, deserialize_with = "deserialize_metadata_string")]
    pub archive_sha256: Option<String>,
    #[serde(default)]
    pub binary_sha256: String,
    #[serde(default, deserialize_with = "deserialize_metadata_string")]
    pub source_commit: Option<String>,
    /// Archive-side binary file name, the full-triple name.
    pub binary_file: String,
    #[serde(default)]
    pub protocol_range: Option<ProtocolRange>,
    #[serde(default, deserialize_with = "deserialize_capabilities")]
    pub capabilities: BTreeMap<String, String>,
    #[serde(default)]
    pub files: Vec<BuildFile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProtocolRange {
    pub minimum: u32,
    pub maximum: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BuildFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

fn deserialize_metadata_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

fn deserialize_capabilities<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Capabilities;
    impl<'de> serde::de::Visitor<'de> for Capabilities {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("unique capability names")
        }
        fn visit_map<M>(self, mut access: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: serde::de::MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((name, version)) = access.next_entry::<String, String>()? {
                if values.insert(name, version).is_some() {
                    return Err(serde::de::Error::custom("duplicate runtime capability"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Capabilities)
}

impl BuildMetadata {
    /// Resolve the legacy `triple` alias without mixing different values.
    fn target_triple(&self) -> Result<&str> {
        match (self.triple.as_deref(), self.target_triple.as_deref()) {
            (Some(legacy), Some(canonical)) => {
                anyhow::ensure!(
                    legacy == canonical,
                    "release metadata carries conflicting target triples"
                );
                Ok(legacy)
            }
            (Some(value), None) | (None, Some(value)) => Ok(value),
            (None, None) => bail!("release metadata has no target triple"),
        }
    }

    /// Resolve the archive digest. In schema 1, legacy `sha256` is an archive
    /// digest; it must never be reused as the unpacked binary digest.
    fn archive_sha256(&self) -> Result<&str> {
        match (self.sha256.as_deref(), self.archive_sha256.as_deref()) {
            (Some(legacy), Some(canonical)) => {
                anyhow::ensure!(
                    legacy == canonical,
                    "release metadata carries conflicting archive checksums"
                );
                Ok(legacy)
            }
            (Some(value), None) | (None, Some(value)) => Ok(value),
            (None, None) => bail!("release metadata has no archive sha256"),
        }
    }

    fn binary_sha256(&self) -> Result<&str> {
        anyhow::ensure!(
            !self.binary_sha256.is_empty(),
            "release metadata has no binary sha256"
        );
        Ok(&self.binary_sha256)
    }

    fn validate(&self, required: &str, triple: &str) -> Result<()> {
        anyhow::ensure!(
            self.schema_version == 1 || self.schema_version == 2,
            "unsupported runtime metadata schema {}",
            self.schema_version
        );
        anyhow::ensure!(
            valid_version(&self.runtime_version) && self.runtime_version == required,
            "release metadata targets runtime {}, expected {required}",
            self.runtime_version
        );
        anyhow::ensure!(
            self.tag == format!("v{required}"),
            "release metadata has an invalid tag"
        );
        anyhow::ensure!(
            self.target_triple()? == triple,
            "release metadata targets triple {}, expected {triple}",
            self.target_triple()?
        );
        anyhow::ensure!(
            valid_sha256(self.binary_sha256()?),
            "release metadata has an invalid sha256"
        );
        anyhow::ensure!(
            self.binary_file == release_binary_name(triple),
            "release metadata names an unexpected binary file: {}",
            self.binary_file
        );
        anyhow::ensure!(
            valid_sha256(self.archive_sha256()?),
            "release metadata has an invalid archive sha256"
        );
        let archive_name = format!("rsrs-runtime-{triple}.tar.gz");
        match self.archive_file.as_deref() {
            Some(name) => anyhow::ensure!(
                name == archive_name,
                "release metadata names an unexpected archive: {name}"
            ),
            None => anyhow::ensure!(
                self.schema_version == 1,
                "release metadata is missing archive_file"
            ),
        }
        if self.schema_version == 2 {
            self.validate_schema2()?;
        }
        Ok(())
    }

    fn validate_schema2(&self) -> Result<()> {
        let commit = self
            .source_commit
            .as_deref()
            .context("release metadata is missing source_commit")?;
        anyhow::ensure!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "release metadata has an invalid source_commit"
        );
        let protocol = self
            .protocol_range
            .as_ref()
            .context("release metadata is missing protocol_range")?;
        anyhow::ensure!(
            protocol.minimum > 0
                && protocol.maximum >= protocol.minimum,
            "release metadata has an invalid protocol range"
        );
        anyhow::ensure!(
            !self.files.is_empty() && self.files.len() <= 4096,
            "release metadata has an invalid file count"
        );

        let mut paths = BTreeSet::new();
        let mut binary_seen = false;
        let mut total_size = 0u64;
        let binary_hash = self.binary_sha256()?;
        for file in &self.files {
            anyhow::ensure!(
                safe_metadata_path(&file.path),
                "unsafe release metadata path: {}",
                file.path
            );
            anyhow::ensure!(
                paths.insert(file.path.to_ascii_lowercase()),
                "duplicate release metadata path: {}",
                file.path
            );
            anyhow::ensure!(
                file.size > 0 && file.size <= MAX_ENTRY_BYTES,
                "release metadata file has invalid size: {}",
                file.path
            );
            anyhow::ensure!(
                valid_sha256(&file.sha256),
                "invalid release metadata sha256: {}",
                file.path
            );
            total_size += file.size;
            anyhow::ensure!(
                total_size <= MAX_TOTAL_BYTES,
                "release files exceed extraction limit"
            );
            if file.path == self.binary_file {
                anyhow::ensure!(
                    file.sha256.eq_ignore_ascii_case(binary_hash),
                    "release metadata files disagree with the binary digest"
                );
                binary_seen = true;
            }
        }
        anyhow::ensure!(
            binary_seen,
            "release metadata files omit the runtime binary"
        );
        Ok(())
    }

    fn verify_files(&self, stage: &Path) -> Result<()> {
        if self.schema_version < 2 {
            return Ok(());
        }
        let mut expected = BTreeSet::new();
        for file in &self.files {
            let name = if file.path == self.binary_file && !stage.join(&file.path).exists() {
                RUNTIME_BINARY
            } else {
                file.path.as_str()
            };
            if file.path.starts_with("core-notices/") && !stage.join(name).try_exists()? {
                continue;
            }
            expected.insert(stage.join(name));
            let path = regular_install_file(stage, name)?;
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("release file is missing: {}", file.path))?;
            anyhow::ensure!(
                metadata.is_file(),
                "release file is not a regular file: {}",
                file.path
            );
            anyhow::ensure!(
                metadata.len() == file.size,
                "release file size mismatch: {}",
                file.path
            );
            let actual = sha256_file(&path)?;
            anyhow::ensure!(
                actual.eq_ignore_ascii_case(&file.sha256),
                "release file checksum mismatch: {}",
                file.path
            );
        }
        // Version directories are immutable payloads. Extra files must not
        // become loader inputs outside the authenticated release inventory.
        let mut directories = vec![stage.to_path_buf()];
        let mut entries = 0usize;
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                entries += 1;
                anyhow::ensure!(entries <= 4096, "installed runtime has too many entries");
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    directories.push(entry.path());
                } else {
                    anyhow::ensure!(
                        kind.is_file() && expected.remove(&entry.path()),
                        "unexpected runtime payload entry: {}",
                        entry.path().display()
                    );
                }
            }
        }
        anyhow::ensure!(expected.is_empty(), "runtime release files disappeared");
        Ok(())
    }
}
