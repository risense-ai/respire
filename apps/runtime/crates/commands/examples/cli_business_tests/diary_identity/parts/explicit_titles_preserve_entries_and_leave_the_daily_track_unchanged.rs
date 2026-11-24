/// Explicit titles identify independent entries instead of silently naming a daily track.
#[test]
fn explicit_titles_preserve_entries_and_leave_the_daily_track_unchanged() -> anyhow::Result<()> {
    for mode in ["concise", "verbose"] {
        let mut fixture = Fixture::new()?;
        std::fs::write(
            fixture.home.path().join("data/agent.json"),
            serde_json::to_vec(&json!({"diary_mode":mode}))?,
        )?;
        fixture.start()?;
        for has_track in [false, true] {
            let existing = if has_track {
                let result = fixture.remember("untitled-existing-event")?;
                let id = result["summary"]["id"]
                    .as_str()
                    .context("missing diary id")?;
                Some((id.to_owned(), fixture.show(id)?))
            } else {
                None
            };
            for importance in ["trivial", "important"] {
                let title = format!("explicit-{mode}-{has_track}-{importance}");
                let body = format!("body-{title}");
                let receipt = fixture.run(&[
                    "remember",
                    &body,
                    "--title",
                    &title,
                    "--importance",
                    importance,
                    "--force",
                ])?;
                let id = receipt["summary"]["id"]
                    .as_str()
                    .context("missing entry id")?;
                let saved = fixture.show(id)?;
                assert_eq!(receipt["details"], saved);
                assert_eq!(saved["title"], title);
                assert!(saved["content"]
                    .as_str()
                    .context("missing body")?
                    .contains(&body));
                assert!(receipt["items"]
                    .as_array()
                    .context("missing items")?
                    .iter()
                    .any(|item| item["value"] == format!("id={id}")));
                if let Some((track_id, before)) = &existing {
                    assert_ne!(id, track_id);
                    assert_eq!(fixture.show(track_id)?, *before);
                }
            }
        }
    }
    support::legacy_receipt("diary_identity::explicit_titles_preserve_entries_and_leave_the_daily_track_unchanged", "candidate_runtime")?;
    Ok(())
}

/// Every successful write action returns a public entry, while blank titles retain aggregation.
#[test]
fn remember_public_payload_covers_created_appended_and_merged() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.start()?;
    let created = fixture.remember("first public diary event")?;
    let id = created["summary"]["id"]
        .as_str()
        .context("missing created id")?;
    assert_eq!(created["summary"]["action"], "created");
    assert_eq!(created["details"], fixture.show(id)?);
    let appended = fixture.run(&["remember", "second public diary event", "--title", " \t "])?;
    assert_eq!(appended["summary"]["action"], "diary_appended");
    assert_eq!(appended["summary"]["id"], id);
    assert_eq!(appended["details"], fixture.show(id)?);
    let source = fixture.run(&[
        "remember",
        "merge source event",
        "--title",
        "merge source",
        "--importance",
        "important",
        "--force",
    ])?;
    let source_id = source["summary"]["id"]
        .as_str()
        .context("missing source id")?;
    let merged = fixture.run(&[
        "remember",
        "merged public event",
        "--title",
        "merged title",
        "--importance",
        "important",
        "--merge-ids",
        source_id,
    ])?;
    assert_eq!(merged["summary"]["action"], "merged");
    let merged_id = merged["summary"]["id"]
        .as_str()
        .context("missing merged id")?;
    assert_eq!(merged["details"], fixture.show(merged_id)?);
    for result in [&created, &appended, &source, &merged] {
        let details = result["details"]
            .as_object()
            .context("missing public entry")?;
        for internal in [
            "ciphertext",
            "nonce",
            "wrapped_urk",
            "embedding",
            "embedding_enc",
            "local_embedding",
            "local_artifact",
            "token",
            "password",
        ] {
            assert!(
                !details.contains_key(internal),
                "public result contains {internal}"
            );
        }
        assert_eq!(result["details"]["id"], result["summary"]["id"]);
    }
    support::legacy_receipt("diary_identity::remember_public_payload_covers_created_appended_and_merged", "candidate_runtime")?;
    Ok(())
}

/// Client-only RPC failures remain one JSON document with an actionable diagnostic.
#[test]
fn missing_entry_and_usage_failures_have_machine_diagnostics() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.start()?;
    for args in [
        vec!["--client-only", "--json", "show", "no-such-id"],
        vec![
            "--client-only",
            "--json",
            "remember",
            "synthetic",
            "--unknown-option",
        ],
    ] {
        let output = fixture.business_output(&args)?;
        assert!(!output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(result["status"], "fail");
        let errors = result["errors"].as_array().context("missing error list")?;
        assert!(!errors.is_empty());
        assert!(errors
            .iter()
            .all(|error| error.as_str().is_some_and(|s| !s.trim().is_empty())));
        if args.contains(&"--unknown-option") {
            assert_eq!(output.status.code(), Some(2));
            assert!(!errors[0]
                .as_str()
                .context("missing concise error")?
                .contains('\n'));
            assert!(result["details"]["usage"]
                .as_str()
                .context("missing usage")?
                .contains("Usage:"));
        } else {
            assert_eq!(output.status.code(), Some(1));
            assert!(errors[0]
                .as_str()
                .context("missing not-found diagnostic")?
                .contains("not found"));
        }
    }
    if let Some(binary) = support::legacy_cli()? {
        let before = serde_json::to_value(fixture.store()?.all(true)?)?;
        let owner = fixture.runtime_owner.borrow().clone();
        let rejected = fixture.command_for(&binary, &["--direct", "--json", "show", "no-such-id"])
            .output()?;
        assert!(!rejected.status.success());
        let failure: Value = serde_json::from_slice(&rejected.stdout)?;
        assert_eq!(failure["status"], "fail");
        assert!(failure["errors"].as_array().context("old direct diagnostic missing")?
            .iter().any(|error| error.as_str().is_some_and(|error|
                error.contains("the local runtime is running"))));
        assert_eq!(serde_json::to_value(fixture.store()?.all(true)?)?, before);
        assert!(fixture.owns_runtime()?);
        assert_eq!(*fixture.runtime_owner.borrow(), owner);
    }
    // --direct forwards through RPC; keep the verified standalone owner alive.
    let output = fixture
        .command(&["--direct", "--json", "show", "no-such-id"])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["status"], "fail");
    assert!(result["errors"][0]
        .as_str()
        .context("missing direct diagnostic")?
        .contains("not found"));
    support::legacy_receipt("diary_identity::missing_entry_and_usage_failures_have_machine_diagnostics", "candidate_runtime")?;
    Ok(())
}

/// Public retitling, including a blank title, must not break daily append identity.
#[test]
fn public_title_edit_and_blank_title_still_append_without_changing_metadata() -> anyhow::Result<()>
{
    let mut fixture = Fixture::new()?;
    fixture.start()?;
    let first = fixture.remember("event-original")?;
    let id = first["summary"]["id"]
        .as_str()
        .context("synthetic response string missing")?;
    fixture.run(&[
        "update",
        id,
        "--title",
        "Renamed synthetic diary",
        "--tags",
        "keep,edited",
        "--kind",
        "context",
        "--importance",
        "important",
    ])?;
    let before = fixture.show(id)?;
    let appended = fixture.remember("event-after-title-edit")?;
    assert_eq!(appended["summary"]["id"], id);
    assert_eq!(appended["summary"]["title"], "Renamed synthetic diary");
    let after = fixture.show(id)?;
    for (field, value) in before
        .as_object()
        .context("synthetic entry is not an object")?
    {
        if !["content", "updated_at", "modified_by"].contains(&field.as_str()) {
            assert_eq!(&after[field], value, "metadata {field} changed");
        }
    }
    let content = after["content"]
        .as_str()
        .context("synthetic response string missing")?;
    assert_eq!(content.matches("event-original").count(), 1);
    assert_eq!(content.matches("event-after-title-edit").count(), 1);
    fixture.run(&["update", id, "--title", ""])?;
    fixture.remember("event-after-blank-title")?;
    let blank = fixture.show(id)?;
    assert_eq!(blank["title"], "");
    assert!(blank["content"]
        .as_str()
        .context("synthetic response string missing")?
        .contains("event-after-blank-title"));
    support::legacy_receipt("diary_identity::public_title_edit_and_blank_title_still_append_without_changing_metadata", "candidate_runtime")?;
    Ok(())
}

/// Stable and legacy tracks merge atomically without crossing author/day boundaries.
#[test]
fn canonical_identity_merges_legacy_tracks_without_touching_other_users_or_days(
) -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let day = day();
    let title = format!("\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} {day}");
    let id = track_id("synthetic-diary", &day);
    let canonical = entry(
        &id,
        "synthetic-diary",
        "Renamed canonical",
        "canonical-event",
    );
    fixture.seed_with_owner(&canonical, false, "server-routing-owner")?;
    fixture.seed_with_owner(
        &entry("legacy-today", "synthetic-diary", &title, "legacy-event"),
        false,
        "server-routing-owner",
    )?;
    let foreign = fixture.seed_with_owner(
        &entry(
            &track_id("another-user", &day),
            "another-user",
            &title,
            "foreign-event",
        ),
        false,
        "server-routing-owner",
    )?;
    let previous = fixture.seed(
        &entry(
            &track_id("synthetic-diary", "2000-01-01"),
            "synthetic-diary",
            "\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} 2000-01-01",
            "previous-day-event",
        ),
        false,
    )?;
    fixture.start()?;
    let result = fixture.remember("fresh-event")?;
    assert_eq!(result["summary"]["id"], id);
    let saved = fixture.show(&id)?;
    assert_eq!(saved["title"], canonical.title);
    assert_eq!(saved["tags"], json!(canonical.tags));
    assert_eq!(saved["importance"], "normal");
    assert_eq!(saved["emotion"], 0.75);
    assert_eq!(saved["parent_id"], canonical.parent_id);
    assert_eq!(saved["device"], canonical.device);
    for marker in ["canonical-event", "legacy-event", "fresh-event"] {
        assert_eq!(
            saved["content"]
                .as_str()
                .context("synthetic response string missing")?
                .matches(marker)
                .count(),
            1
        );
    }
    fixture.stop();
    let rows = fixture.store()?.all(true)?;
    assert_eq!(
        rows.iter()
            .find(|row| row.id == id)
            .context("canonical row missing")?
            .user,
        "server-routing-owner"
    );
    assert!(
        rows.iter()
            .find(|row| row.id == "legacy-today")
            .context("legacy row missing")?
            .deleted
    );
    for original in [foreign, previous] {
        let after = rows
            .iter()
            .find(|row| row.id == original.id)
            .context("unrelated row missing")?;
        assert_eq!(after.ciphertext, original.ciphertext);
        assert!(!after.deleted);
    }
    support::legacy_receipt("diary_identity::canonical_identity_merges_legacy_tracks_without_touching_other_users_or_days", "candidate_runtime")?;
    Ok(())
}

/// Tombstoned content must stay deleted while legacy fallback and recreation work.
#[test]
fn deleted_stable_track_uses_legacy_fallback_and_deleted_history_is_not_resurrected(
) -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let day = day();
    let id = track_id("synthetic-diary", &day);
    let title = format!("\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} {day}");
    fixture.seed(
        &entry(
            &id,
            "synthetic-diary",
            "deleted renamed track",
            "deleted-event",
        ),
        true,
    )?;
    fixture.seed(
        &entry(
            "legacy-live",
            "synthetic-diary",
            &title,
            "legacy-live-event",
        ),
        false,
    )?;
    fixture.start()?;
    let result = fixture.remember("append-to-legacy")?;
    assert_eq!(result["summary"]["id"], "legacy-live");
    let saved = fixture.show("legacy-live")?;
    assert!(saved["content"]
        .as_str()
        .context("synthetic response string missing")?
        .contains("legacy-live-event"));
    assert!(!saved["content"]
        .as_str()
        .context("synthetic response string missing")?
        .contains("deleted-event"));
    fixture.run(&["forget", "legacy-live"])?;
    let recreated = fixture.remember("only-new-event")?;
    assert_eq!(recreated["summary"]["id"], id);
    let saved = fixture.show(&id)?;
    let content = saved["content"]
        .as_str()
        .context("synthetic response string missing")?;
    assert!(content.contains("only-new-event"));
    assert!(!content.contains("deleted-event") && !content.contains("legacy-live-event"));
    support::legacy_receipt("diary_identity::deleted_stable_track_uses_legacy_fallback_and_deleted_history_is_not_resurrected", "candidate_runtime")?;
    Ok(())
}

