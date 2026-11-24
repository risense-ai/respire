#[cfg(test)]
mod model_menu_tests {
    use super::*;

    #[test]
    fn small_terminal_keeps_weight_actions_visible() -> Result<()> {
        let shared = Arc::new(Mutex::new(Live::empty()));
        let mut app = App::new(shared);
        app.live.connected = true;
        app.page = Page::Model;
        let variants = (0..12).map(|index| serde_json::json!({
            "name":format!("runtime-{index}"),"size":7340032,"state":"present"
        })).collect::<Vec<_>>();
        let count = variants.len();
        app.live.model_settings = serde_json::json!({"engine":"cpu","model":"runtime-0",
            "variants":variants});
        let mut form = model_weights::Form::new(&app.live.model_settings)?;
        form.cursor = count + 1;
        app.overlay = Overlay::ModelWeights(form);
        let backend = ratatui::backend::TestBackend::new(70, 20);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| draw(frame, &app))?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        // Wide characters occupy two cells in TestBackend, including a blank continuation.
        let compact = screen.replace(' ', "");
        assert!(
            compact.contains("Validateandload")
                || compact.contains("\u{6821}\u{9a8c}\u{5e76}\u{52a0}\u{8f7d}"),
            "{screen}"
        );
        Ok(())
    }

    #[test]
    fn selection_commands_check_errors_read_back_and_always_retire_the_session() -> Result<()> {
        let mut calls = Vec::new();
        let settings = serde_json::json!({"engine":"cpu","model":"fp16","variants":[]});
        let actual = save_model_settings_with("cpu", Some("fp16"), |args| {
            calls.push(args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
            Ok(
                serde_json::json!({"status":"ok","summary":if args[1] == "load" {
                serde_json::json!({"loaded":true,"validated":true,"saved":true})
            } else { settings.clone() }}),
            )
        })
        .map_err(anyhow::Error::msg)?;
        assert_eq!(actual, settings);
        assert_eq!(
            calls,
            vec![
                vec!["model", "load", "cpu", "--model", "fp16"],
                vec!["model", "engine"]
            ]
        );
        let mut failures = 0;
        assert!(save_model_settings_with("cpu", Some("fp16"), |_| {
            failures += 1;
            Ok(serde_json::json!({"status":"fail","errors":["reset failed"]}))
        })
        .is_err());
        assert_eq!(failures, 1);
        assert!(save_model_settings_with("gpu", Some("fp16"), |_| Ok(
            serde_json::json!({"status":"ok","summary":{"engine":"cpu","model":"quantized"}})
        ))
        .is_err());
        assert!(probe_notice(serde_json::json!({"summary":{"ready":false}})).is_err());
        assert!(probe_notice(serde_json::json!({"summary":{"ready":true}})).is_err());
        Ok(())
    }

    #[test]
    fn picker_cancel_missing_weights_and_download_confirmation_do_not_write() -> Result<()> {
        let shared = Arc::new(Mutex::new(Live::empty()));
        let mut app = App::new(shared);
        app.live.connected = true;
        app.live.model_settings = serde_json::json!({"engine":"cpu","model":"runtime-only",
            "variants":[{"name":"runtime-only","size":7340032,"state":"missing"}]});
        app.overlay = Overlay::ModelWeights(model_weights::Form::new(&app.live.model_settings)?);
        assert!(!handle(&mut app, KeyCode::Esc));
        assert!(matches!(app.overlay, Overlay::None));
        let mut form = model_weights::Form::new(&app.live.model_settings)?;
        form.cursor = 2;
        app.overlay = Overlay::ModelWeights(form);
        model_weights_key(&mut app, KeyCode::Enter);
        assert!(
            matches!(app.overlay, Overlay::ModelWeights(_)),
            "missing weights must not be saved"
        );
        app.live.model_settings["variants"][0]["state"] = serde_json::json!("present");
        model_weights_key(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.overlay,
            Overlay::Confirm {
                cursor: 1,
                kind: ConfirmKind::Weights(_, _),
                ..
            }
        ));
        confirm_key(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Overlay::None));
        let mut form = model_weights::Form::new(&app.live.model_settings)?;
        form.cursor = 3;
        app.overlay = Overlay::ModelWeights(form);
        model_weights_key(&mut app, KeyCode::Enter);
        let Overlay::Confirm {
            text,
            cursor: 1,
            kind: ConfirmKind::InstallVariant(engine, name),
        } = &app.overlay
        else {
            anyhow::bail!("download must require confirmation defaulting to cancel");
        };
        assert_eq!(engine, "cpu");
        assert_eq!(name, "runtime-only");
        assert!(text.contains("7 MiB"));
        assert!(text.contains("543 MiB"));
        confirm_key(&mut app, KeyCode::Enter);
        assert!(app
            .outcome
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .is_none());
        assert_eq!(app.live.model_settings["model"], "runtime-only");
        Ok(())
    }

    #[test]
    fn mirror_cancel_restart_keys_and_live_progress_match_the_menu() -> anyhow::Result<()> {
        let _lock = crate::TEST_ENV_LOCK
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let dir = tempfile::tempdir()?;
        let previous = respire::env::var("RSRS_DATA_DIR").ok();
        std::env::set_var("RSRS_DATA_DIR", dir.path());
        let result = (|| -> anyhow::Result<()> {
            std::fs::write(
                dir.path().join("agent.json"),
                serde_json::to_vec(&serde_json::json!({
                    "model_mirror": "http://127.0.0.1:9999"
                }))?,
            )?;
            let mut app = App::new(Arc::new(Mutex::new(Live::empty())));
            for width in [40, 100] {
                let rows = home_body(&app, width)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(rows.contains(&runtime_version_label(&app.live)));
            }
            let rows = version_body(&app)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(rows.contains(&runtime_version_label(&app.live)));
            app.live.runtime_version = "1.0.9".to_owned();
            let rows = home_body(&app, 100)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(rows.contains("CLI "));
            assert!(rows.contains(crate::app_version::embedded()));
            assert!(rows.contains("Runtime 1.0.9"));
            let rows = version_body(&app)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(rows.contains("Runtime 1.0.9"));
            app.page = Page::Model;
            app.cursor = 2;
            model_key(&mut app, KeyCode::Enter);
            let Overlay::ModelMirror(input) = &app.overlay else {
                anyhow::bail!("mirror picker did not open");
            };
            assert_eq!(input.text, "http://127.0.0.1:9999");
            let picker =
                overlay_lines(&app).ok_or_else(|| anyhow::anyhow!("mirror picker has no rows"))?;
            assert!(picker
                .iter()
                .any(|line| line.to_string().contains("hf-mirror.com")));
            app.live.model_progress = serde_json::json!({"id":"fixture-task","active":true,"phase":"download","item":"model_quantized.onnx","done":25,"total":100});
            let rows = model_body(&app)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(rows.contains("25.0%"));
            assert!(rows.contains("model_quantized.onnx"));
            app.live.inference = serde_json::json!({"active":true,"queued":3,"phase":"loading"});
            let busy_rows = model_body(&app)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            let inference_text = crate::progress::inference_progress_text(&app.live.inference)
                .ok_or_else(|| anyhow::anyhow!("missing inference status"))?;
            assert!(busy_rows.contains(&inference_text));
            app.live.inference = serde_json::json!({"host_recovery_required":true});
            let stalled_rows = model_body(&app)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            let stalled_text = crate::progress::inference_progress_text(&app.live.inference)
                .ok_or_else(|| anyhow::anyhow!("missing stalled status"))?;
            assert!(stalled_rows.contains(&stalled_text));
            app.cursor = 9 + model_engines().len() + usize::from(cfg!(windows));
            model_key(&mut app, KeyCode::Enter);
            assert!(
                matches!(&app.overlay, Overlay::Confirm { kind: ConfirmKind::CancelModel(id), .. } if id == "fixture-task")
            );
            app.cursor += 1;
            model_key(&mut app, KeyCode::Enter);
            assert!(
                matches!(&app.overlay, Overlay::Confirm { kind: ConfirmKind::RestartM3(id, mirror), .. } if id == "fixture-task" && mirror == "http://127.0.0.1:9999")
            );
            app.cursor += 1;
            model_key(&mut app, KeyCode::Enter);
            assert!(matches!(&app.overlay, Overlay::RecallTest(_)));
            app.overlay = Overlay::None;
            app.cursor += 1;
            app.live.model_settings = Value::Null;
            model_key(&mut app, KeyCode::Enter);
            assert!(matches!(app.overlay, Overlay::None));
            app.live.model_settings = serde_json::json!({"engine":"cpu","model":"runtime-only",
                "variants":[{"name":"runtime-only","size":7340032,"state":"present"}]});
            model_key(&mut app, KeyCode::Enter);
            assert!(matches!(app.overlay, Overlay::ModelWeights(_)));
            app.overlay = Overlay::None;
            app.cursor += 2;
            model_key(&mut app, KeyCode::Enter);
            assert!(matches!(app.page, Page::Home));
            Ok(())
        })();
        match previous {
            Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        result
    }
}
