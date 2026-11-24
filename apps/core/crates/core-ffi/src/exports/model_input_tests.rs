    use super::*;
use super::input_stream::{rs_core_model_input_abort, rs_core_model_input_append, rs_core_model_input_begin, rs_core_model_input_commit};
    unsafe fn free(output: &mut Buffer) { unsafe { rs_core_buffer_free(output) }; }
    #[test]
    fn tickets_preflight_consumption_and_abort() -> anyhow::Result<()> {
        let _serial = model_input::TEST_LOCK.lock().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut a = Core::default();
        let mut b = Core::default();
        let mut output = Buffer::empty();
        let mut ticket = 0;
        unsafe {
            assert_eq!(rs_core_model_input_begin(&mut a, 8, &mut ticket, &mut output), OK); free(&mut output);
            let mut second = 42;
            assert_eq!(rs_core_model_input_begin(&mut a, 8, &mut second, &mut output), INVALID_ARGUMENT); free(&mut output);
            assert_eq!(second, 0);
            assert_eq!(rs_core_model_input_append(&mut b, ticket, 0, b"abcd".as_ptr(), 4, &mut output), INVALID_ARGUMENT); free(&mut output);
            assert_eq!(rs_core_model_input_append(&mut a, ticket, 0, ptr::null(), 1, &mut output), INVALID_ARGUMENT); free(&mut output);
            assert_eq!(rs_core_model_input_append(&mut a, ticket, 0, b"abcd".as_ptr(), 4, &mut output), OK); free(&mut output);
            // Structural preflight failure leaves ticket pending.
            assert_eq!(rs_core_model_input_commit(&mut a, ticket, b"{}".as_ptr(), 2, ptr::null(), 0, ptr::null_mut()), INVALID_ARGUMENT);
            assert!(a.pending_input.is_some());
            // Incomplete matching commit consumes the ticket.
            assert_ne!(rs_core_model_input_commit(&mut a, ticket, b"{}".as_ptr(), 2, ptr::null(), 0, &mut output), OK); free(&mut output);
            assert!(a.pending_input.is_none());
            let old = ticket;
            assert_eq!(rs_core_model_input_begin(&mut a, 8, &mut ticket, &mut output), OK); free(&mut output);
            assert_ne!(old, ticket);
            assert_eq!(rs_core_model_input_commit(&mut a, old, b"{}".as_ptr(), 2, ptr::null(), 0, &mut output), INVALID_ARGUMENT); free(&mut output);
            assert_eq!(rs_core_model_input_abort(&mut a, old, &mut output), OK); free(&mut output);
            assert_eq!(a.pending_input.as_ref().map(|pending| pending.ticket()), Some(ticket));
            assert_eq!(rs_core_model_input_append(&mut a, ticket, 0, b"abcdORTM".as_ptr(), 8, &mut output), OK); free(&mut output);
            // Malformed config consumes complete input without touching native ORT.
            assert_eq!(rs_core_model_input_commit(&mut a, ticket, b"{".as_ptr(), 1, ptr::null(), 0, &mut output), INVALID_ARGUMENT); free(&mut output);
            assert!(a.pending_input.is_none());
            assert_eq!(rs_core_model_input_abort(&mut a, ticket, &mut output), OK); free(&mut output);
            assert_eq!(rs_core_model_input_abort(&mut a, ticket, &mut output), OK); free(&mut output);
            assert_eq!(rs_core_model_input_begin(&mut a, 8, &mut ticket, &mut output), OK); free(&mut output);
            assert_eq!(rs_core_model_input_append(&mut a, ticket, 0, b"abcdORTM".as_ptr(), 8, &mut output), OK); free(&mut output);
            let unsupported = br#"{"model":"m3","engine":"cpu","cpu_owned_ort_experiment":false,"asset_id":"fixture"}"#;
            assert_ne!(rs_core_model_input_commit(&mut a, ticket, unsupported.as_ptr(), unsupported.len(), b"{}".as_ptr(), 2, &mut output), OK); free(&mut output);
            assert!(a.pending_input.is_none());
            #[cfg(feature = "cpu-owned-ort-experiment")]
            {
                use sha2::{Digest, Sha256};
                // A broken digest is rejected; the correct combined digest
                // passes despite immediate reuse of the borrowed scratch.
                for (model_hash, expected_error) in [
                    ("0".repeat(64), "host-supplied model_sha256 differs"),
                    (format!("{:x}", Sha256::digest(b"abcdORTM")), "host-supplied tokenizer_sha256 differs"),
                ] {
                    assert_eq!(rs_core_model_input_begin(&mut a, 8, &mut ticket, &mut output), OK); free(&mut output);
                    let mut chunk = *b"abcd";
                    assert_eq!(rs_core_model_input_append(&mut a, ticket, 0, chunk.as_ptr(), 4, &mut output), OK); free(&mut output);
                    chunk.copy_from_slice(b"ORTM");
                    assert_eq!(rs_core_model_input_append(&mut a, ticket, 4, chunk.as_ptr(), 4, &mut output), OK); free(&mut output);
                    let config = serde_json::to_vec(&json!({
                        "model": "m3", "engine": "cpu", "cpu_owned_ort_experiment": true,
                        "asset_id": "fixture", "execution_timeout_secs": 5,
                        "model_sha256": model_hash, "tokenizer_sha256": "0".repeat(64)
                    }))?;
                    assert_eq!(rs_core_model_input_commit(&mut a, ticket, config.as_ptr(), config.len(), b"{}".as_ptr(), 2, &mut output), ENGINE_FAILURE);
                    let message = serde_json::from_slice::<Value>(std::slice::from_raw_parts(output.data, output.len));
                    free(&mut output);
                    assert!(message?.to_string().contains(expected_error));
                    assert!(a.pending_input.is_none());
                }
            }
            assert_eq!(rs_core_model_input_begin(&mut a, 8, &mut ticket, &mut output), OK); free(&mut output);
        }
        // Ordinary Core Drop releases the final Pending. Global accounting is
        // tested separately under the same staging-test lock.
        drop(a);
        Ok(())
    }
