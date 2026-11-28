//! Isolated synthetic assets only; no accounts, credentials or inference files.
use super::*;
use std::time::Duration;

struct Fixture {
    _temporary: tempfile::TempDir,
    profile: PathBuf,
    root: PathBuf,
    ownership: LibraryLock,
    assets: LibraryLock,
    binding: Binding,
    active: Active,
    pending: Option<profiles::Selection>,
    cancel_after_first_move: bool,
    legacy_assets: bool,
    wrong_asset_lock: bool,
}

impl Fixture {
    fn new() -> Result<Self> {
        let (temporary, profile) = secure_temp()?;
        let owned = owned_assets::OwnedAssets::provision(&profile, &profile)?;
        let root = owned.path().join("inactive");
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(&root)?;
        }
        #[cfg(windows)]
        fs::create_dir(&root)?;
        let assets = owned.acquire_ownership(Duration::ZERO)?;
        for (name, bytes) in [
            ("model_quantized.onnx", b"synthetic weights".as_slice()),
            ("tokenizer.json", b"synthetic tokenizer".as_slice()),
        ] {
            let file = root.join(name);
            fs::write(&file, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
            }
        }
        fs::copy(
            root.join("model_quantized.onnx"),
            root.join("active-weights"),
        )?;
        fs::copy(root.join("tokenizer.json"), root.join("active-tokenizer"))?;
        let ownership = LibraryLock::acquire(&profile, Duration::ZERO)?;
        let binding = Binding {
            owner: "synthetic-owner".into(),
            canonical_profile: profile.clone(),
            instance: uuid::Uuid::new_v4(),
            generation: 7,
            vault_epoch: uuid::Uuid::new_v4(),
        };
        let mut fixture = Self {
            _temporary: temporary,
            profile,
            root,
            ownership,
            assets,
            binding,
            active: Active::Uncertain,
            pending: None,
            cancel_after_first_move: false,
            legacy_assets: false,
            wrong_asset_lock: false,
        };
        fixture.active = Active::ImplicitM3(fixture.implicit_profile()?);
        Ok(fixture)
    }

    fn registry(&self) -> Result<Registry> {
        let resource = |name: &str| -> Result<profiles::Resource> {
            let path = if self.legacy_assets
                && matches!(name, "model_quantized.onnx" | "tokenizer.json")
            {
                self.profile.join("models/bge-m3").join(name)
            } else {
                self.root.join(name)
            };
            Ok(profiles::Resource {
                size: fs::metadata(&path)?.len(),
                sha256: digest_file(File::open(&path)?)?,
                path,
                download: None,
            })
        };
        let model = profiles::ModelProfile {
            id: "inactive".into(),
            embedding: serde_json::Value::Null,
            onnx: serde_json::Value::Null,
            weights: Some(resource("model_quantized.onnx")?),
            tokenizer: Some(resource("tokenizer.json")?),
            companions: Vec::new(),
            isolate_engines: false,
        };
        let mut registry = Registry::default();
        registry.models.insert(model.id.clone(), model);
        Ok(registry)
    }

    fn selection(&self) -> Result<profiles::Selection> {
        let registry = self.registry()?;
        Ok(profiles::Selection {
            model: "inactive".into(),
            backend: "onnx".into(),
            engine: respire_app::core_sdk::onnx::Engine::Cpu,
            variant: None,
            profile: registry.models["inactive"].clone(),
            backend_profile: serde_json::from_value(
                serde_json::json!({"id":"onnx","backend":"onnx"}),
            )?,
            generation: "synthetic-generation".into(),
        })
    }

    fn implicit_profile(&self) -> Result<profiles::ModelProfile> {
        let mut profile = self.selection()?.profile;
        profile.id = "m3".into();
        profile.weights.as_mut().context("fixture weights")?.path =
            self.root.join("active-weights");
        profile
            .tokenizer
            .as_mut()
            .context("fixture tokenizer")?
            .path = self.root.join("active-tokenizer");
        Ok(profile)
    }

    fn request(&self) -> Request {
        Request {
            model_id: "inactive".into(),
        }
    }

    fn unchanged(&self) -> Result<()> {
        assert_eq!(
            fs::read(self.root.join("model_quantized.onnx"))?,
            b"synthetic weights"
        );
        assert_eq!(
            fs::read(self.root.join("tokenizer.json"))?,
            b"synthetic tokenizer"
        );
        assert!(!self
            .root
            .parent()
            .unwrap()
            .join(".runtime-model-removal-v1")
            .exists());
        Ok(())
    }
}

impl Authority for Fixture {
    fn with_current<T>(
        &self,
        _: &Binding,
        action: impl FnOnce(State<'_>) -> Result<T>,
    ) -> Result<T> {
        action(State {
            binding: self.binding.clone(),
            registry: self.registry()?,
            installation_root: self.profile.clone(),
            asset_ownership: if self.wrong_asset_lock {
                &self.ownership
            } else {
                &self.assets
            },
            scopes: vec![Scope {
                profile: &self.profile,
                ownership: &self.ownership,
                active: self.active.clone(),
                pending: self.pending.clone(),
            }],
        })
    }

    fn check(&self, binding: &Binding) -> Result<()> {
        anyhow::ensure!(binding == &self.binding, "scope_mismatch");
        anyhow::ensure!(
            !self.cancel_after_first_move
                || self.root.join("model_quantized.onnx").is_file()
                || !self.root.join("tokenizer.json").is_file(),
            "operation_cancelled_after_first_move"
        );
        Ok(())
    }
}

#[test]
fn removes_only_registered_inactive_files_and_retains_other_assets_and_lock() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join("other-active-model"), b"active bytes")?;
    let receipt = uninstall(&fixture, &fixture.binding, fixture.request())?;
    assert_eq!(receipt.outcome, Outcome::Removed);
    assert_eq!(receipt.detached_files, 2);
    assert!(!receipt.active_changed);
    assert!(!fixture.root.join("model_quantized.onnx").exists());
    assert!(!fixture.root.join("tokenizer.json").exists());
    assert_eq!(
        fs::read(fixture.root.join("other-active-model"))?,
        b"active bytes"
    );
    assert_eq!(
        fs::read(fixture.root.join("active-weights"))?,
        b"synthetic weights"
    );
    assert_eq!(
        fs::read(fixture.root.join("active-tokenizer"))?,
        b"synthetic tokenizer"
    );
    assert!(fixture.ownership.protects(&fixture.profile)?);
    assert!(matches!(fixture.active, Active::ImplicitM3(_)));
    assert_eq!(receipt.deleted_files, 2);
    assert!(receipt.cleanup_error.is_none());
    assert!(!fixture
        .root
        .parent()
        .unwrap()
        .join(".runtime-model-removal-v1")
        .exists());
    Ok(())
}

#[test]
fn active_explicit_or_implicit_selection_is_rejected_without_effects() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.active = Active::Explicit(fixture.selection()?);
    assert!(uninstall(&fixture, &fixture.binding, fixture.request()).is_err());
    fixture.unchanged()?;
    fixture.active = Active::Uncertain;
    assert!(uninstall(&fixture, &fixture.binding, fixture.request()).is_err());
    fixture.unchanged()?;
    fixture.active = Active::ImplicitM3(fixture.implicit_profile()?);
    let request = Request {
        model_id: "m3".into(),
    };
    let error = uninstall(&fixture, &fixture.binding, request)
        .err()
        .context("expected active rejection")?;
    assert!(format!("{error:#}").contains("model_in_use"));
    let mut implicit = fixture.selection()?.profile;
    implicit.id = "m3".into();
    fixture.active = Active::ImplicitM3(implicit);
    assert!(uninstall(&fixture, &fixture.binding, fixture.request()).is_err());
    fixture.unchanged()?;
    fixture.active = Active::ImplicitM3(fixture.implicit_profile()?);
    fixture.pending = Some(fixture.selection()?);
    assert!(uninstall(&fixture, &fixture.binding, fixture.request()).is_err());
    fixture.unchanged()
}

#[test]
fn stale_binding_and_unknown_id_have_no_effects() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut binding = fixture.binding.clone();
    binding.generation += 1;
    assert!(uninstall(&fixture, &binding, fixture.request()).is_err());
    let mut request = fixture.request();
    request.model_id = "unknown".into();
    assert!(uninstall(&fixture, &fixture.binding, request).is_err());
    fixture.unchanged()
}

#[test]
fn cancellation_after_one_move_restores_all_original_bytes() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.cancel_after_first_move = true;
    let error = uninstall(&fixture, &fixture.binding, fixture.request())
        .err()
        .context("expected cancellation")?;
    assert!(format!("{error:#}").contains("assets restored"));
    fixture.unchanged()
}

#[test]
fn ordinary_shared_resource_and_incomplete_transaction_remain_protected() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut registry = fixture.registry()?;
    let mut shared = registry.models["inactive"].clone();
    shared.id = "second-model".into();
    registry.models.insert(shared.id.clone(), shared);
    struct Shared<'a>(&'a Fixture, Registry);
    impl Authority for Shared<'_> {
        fn with_current<T>(
            &self,
            _: &Binding,
            action: impl FnOnce(State<'_>) -> Result<T>,
        ) -> Result<T> {
            action(State {
                binding: self.0.binding.clone(),
                registry: serde_json::from_value(serde_json::to_value(&self.1)?)?,
                installation_root: self.0.profile.clone(),
                asset_ownership: &self.0.assets,
                scopes: vec![Scope {
                    profile: &self.0.profile,
                    ownership: &self.0.ownership,
                    active: Active::ImplicitM3(self.0.implicit_profile()?),
                    pending: None,
                }],
            })
        }
        fn check(&self, binding: &Binding) -> Result<()> {
            self.0.check(binding)
        }
    }
    assert!(uninstall(
        &Shared(&fixture, registry),
        &fixture.binding,
        fixture.request()
    )
    .is_err());
    fixture.unchanged()?;
    let stage = fixture
        .root
        .parent()
        .unwrap()
        .join(".runtime-model-removal-v1");
    fs::create_dir(&stage)?;
    fs::write(
        stage.join("journal.json"),
        b"retained interrupted transaction",
    )?;
    assert!(uninstall(&fixture, &fixture.binding, fixture.request()).is_err());
    assert_eq!(
        fs::read(stage.join("journal.json"))?,
        b"retained interrupted transaction"
    );
    assert_eq!(
        fs::read(fixture.root.join("model_quantized.onnx"))?,
        b"synthetic weights"
    );
    assert_eq!(
        fs::read(fixture.root.join("tokenizer.json"))?,
        b"synthetic tokenizer"
    );
    Ok(())
}

#[test]
fn legacy_assets_are_not_online_removal_candidates() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let legacy = fixture.profile.join("models/bge-m3");
    fs::create_dir_all(&legacy)?;
    fs::copy(
        fixture.root.join("model_quantized.onnx"),
        legacy.join("model_quantized.onnx"),
    )?;
    fs::copy(
        fixture.root.join("tokenizer.json"),
        legacy.join("tokenizer.json"),
    )?;
    fixture.legacy_assets = true;
    let error = uninstall(&fixture, &fixture.binding, fixture.request()).unwrap_err();
    assert!(error.to_string().contains("asset_not_owned"));
    assert_eq!(
        fs::read(legacy.join("model_quantized.onnx"))?,
        b"synthetic weights"
    );
    assert_eq!(
        fs::read(legacy.join("tokenizer.json"))?,
        b"synthetic tokenizer"
    );
    fixture.unchanged()
}

#[test]
fn profile_lock_is_not_a_managed_asset_publisher_lock() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.wrong_asset_lock = true;
    let error = uninstall(&fixture, &fixture.binding, fixture.request()).unwrap_err();
    assert!(error.to_string().contains("managed_asset_lock_mismatch"));
    fixture.unchanged()
}
