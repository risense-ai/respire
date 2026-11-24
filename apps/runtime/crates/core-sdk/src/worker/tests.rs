    #[test]
    fn interrupted_load_reports_uncertainty_without_replaying_or_claiming_rollback() {
        let load =
            super::unconfirmed_model_load("host_load_selection", anyhow::anyhow!("IPC closed"));
        let message = format!("{load:#}");
        assert!(message.contains("load outcome unconfirmed"));
        assert!(message.contains("settings may already be saved"));
        assert!(message.contains("previous model recovery unconfirmed"));
        assert!(message.contains("request was not replayed"));
        let other = super::unconfirmed_model_load("model_probe", anyhow::anyhow!("IPC closed"));
        assert_eq!(other.to_string(), "IPC closed");
    }
    use super::*;
    #[test]
    fn borrowed_frames_keep_the_existing_wire_contract() -> Result<()> {
        let payload = json!({"nested":[null, {"body":"synthetic unicode fixture", "ids":[1,2,3]}]});
        let request = RequestFrame {
            id: u64::MAX,
            operation: "query",
            payload: &payload,
            index_root: None,
            transport: true,
            selection: None,
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request)?;
        let decoded = read_frame(&mut bytes.as_slice())?;
        assert_eq!(decoded["id"].as_u64(), Some(u64::MAX));
        assert_eq!(decoded["operation"], "query");
        assert_eq!(decoded["payload"], payload);
        assert!(decoded["index_root"].is_null());
        assert_eq!(decoded["transport"], true);
        for (key, body) in [
            ("result", ResponseBody::Result { result: &payload }),
            ("callback", ResponseBody::Callback { callback: &payload }),
            (
                "callback_result",
                ResponseBody::CallbackResult {
                    callback_result: &payload,
                },
            ),
            (
                "error",
                ResponseBody::Error {
                    error: String::from("synthetic error"),
                },
            ),
            (
                "callback_error",
                ResponseBody::CallbackError {
                    callback_error: String::from("synthetic error"),
                },
            ),
        ] {
            bytes.clear();
            write_frame(&mut bytes, &ResponseFrame { id: 9, body })?;
            let decoded = read_frame(&mut bytes.as_slice())?;
            assert_eq!(decoded["id"], 9);
            assert_eq!(decoded.as_object().map(|object| object.len()), Some(2));
            if key.ends_with("error") {
                assert_eq!(decoded[key], "synthetic error");
            } else {
                assert_eq!(decoded[key], payload);
            }
        }
        bytes.clear();
        write_frame(
            &mut bytes,
            &HeartbeatFrame {
                heartbeat: &payload,
            },
        )?;
        let decoded = read_frame(&mut bytes.as_slice())?;
        assert_eq!(decoded["heartbeat"], payload);
        assert!(decoded.get("id").is_none());
        Ok(())
    }

    #[test]
    fn private_frames_preserve_values_and_reject_truncation() {
        let value = json!({"id":9,"result":{"text":"synthetic unicode fixture \u{4e2d}\u{6587}"}});
        let mut bytes = Vec::new();
        assert!(write_frame(&mut bytes, &value).is_ok());
        assert_eq!(read_frame(&mut bytes.as_slice()).ok(), Some(value));
        bytes.pop();
        assert!(read_frame(&mut bytes.as_slice()).is_err());
        assert!(read_frame(&mut [0u8; 8].as_slice()).is_err());
    }
