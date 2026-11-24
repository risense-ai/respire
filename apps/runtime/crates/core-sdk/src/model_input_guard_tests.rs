    use super::*;
    #[test]
    #[ignore = "requires the pinned actual ORT model fixture"]
    fn real_stream_commit_cache_budget_and_handle_lifetime() -> Result<()> {
        use std::io::Read;
        fn stable(mut value: Value) -> Value {
            if let Some(fields) = value.as_object_mut() {
                fields.remove("elapsed_ms");
            }
            value
        }
        let root = std::path::PathBuf::from(
            std::env::var_os("RSRS_CORE_TEST_MODEL_DIR").context("explicit fixture required")?,
        );
        let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
        fn fill<'a>(core: &'a mut Core, root: &std::path::Path) -> Result<ModelInput<'a>> {
            let mut file = std::fs::File::open(root.join("onnx/model_quantized.cpu-v1.ort"))?;
            anyhow::ensure!(
                file.metadata()?.len() == 569654128,
                "fixture length mismatch"
            );
            let mut upload = core.begin_model_input(569654128)?;
            let mut chunk = vec![0; 1 << 20];
            loop {
                let n = file.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                upload.append(&chunk[..n])?;
            }
            Ok(upload)
        }
        let config = json!({"model":"m3","asset_id":"actual-stream-budget-fixture","engine":"cpu","execution_timeout_secs":120,"cpu_owned_ort_experiment":true});
        let mut core = Core::new()?;
        fill(&mut core, &root)?.commit(config.clone(), &tokenizer)?;
        // A model-sized new reservation must succeed after publication.
        drop(core.begin_model_input(569654128)?);
        let expected = stable(core.call(
            "model_probe",
            json!({"model":"m3","text":"model input lifecycle"}),
        )?);
        fill(&mut core, &root)?.commit(config.clone(), &tokenizer)?;
        drop(core.begin_model_input(569654128)?);
        assert_eq!(
            stable(core.call(
                "model_probe",
                json!({"model":"m3","text":"model input lifecycle"})
            )?),
            expected
        );
        drop(core);
        let mut next = Core::new()?;
        let mut reuse = config.clone();
        reuse["reuse_current"] = json!(true);
        next.load_model(reuse, &[], &[])?;
        assert_eq!(
            stable(next.call(
                "model_probe",
                json!({"model":"m3","text":"model input lifecycle"})
            )?),
            expected
        );
        let mut invalid = config;
        invalid["asset_id"] = json!("invalid-tokenizer-replacement");
        assert!(fill(&mut next, &root)?
            .commit(invalid, b"bad tokenizer")
            .is_err());
        drop(next.begin_model_input(569654128)?);
        Ok(())
    }

    #[test]
    fn cancelled_and_failed_uploads_allow_new_guard() -> Result<()> {
        let mut core = Core::new()?;
        {
            let mut upload = core.begin_model_input(8)?;
            upload.append(b"part")?;
        }
        let mut upload = core.begin_model_input(8)?;
        assert!(upload.append(b"too many bytes").is_err());
        upload.append(b"abcdORTM")?;
        assert!(upload.commit(json!({"model":"m3","asset_id":"fixture","engine":"cpu","cpu_owned_ort_experiment":true}), b"invalid tokenizer").is_err());
        let incomplete = core.begin_model_input(8)?;
        assert!(incomplete.commit(json!({}), &[]).is_err());
        drop(core.begin_model_input(8)?);
        assert_eq!(core.capabilities()?["model_input_stream_contract"], 1);
        Ok(())
    }
