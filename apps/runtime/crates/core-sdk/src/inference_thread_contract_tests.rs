    use super::*;

    #[test]
    fn sdk_advertises_thread_isolation_and_rejects_invalid_configuration() -> Result<()> {
        let mut core = Core::new()?;
        assert_eq!(core.capabilities()?["inference_thread_contract"], 1);
        for invalid in [
            serde_json::json!(0),
            serde_json::json!(17),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("4"),
        ] {
            let request = serde_json::json!({"model":"m3","asset_id":"invalid-policy","engine":"cpu",
                "execution_timeout_secs":120,"background_intra_threads":invalid});
            let error = core
                .load_model(request, &[], &[])
                .err()
                .ok_or_else(|| anyhow::anyhow!("invalid thread policy unexpectedly accepted"))?;
            assert!(
                error.to_string().contains("background_intra_threads"),
                "unexpected validation error: {error}"
            );
        }
        Ok(())
    }
