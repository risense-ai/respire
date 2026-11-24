"""Pinned native account and diary case names."""



TARGET = "x86_64-unknown-linux-gnu"
CASE = "account::authorization_preserves_and_commits_profiles"
MODULE_COUNTS = {"memory_revision": 6, "diary_identity": 11, "snapshot_lifecycle": 8,
                 "account": 3, "capture_main": 2}
NATIVE_STOP_CASE = "diary_identity::native_stop_waits_and_explicit_force_reaps_real_core_without_replay"
G1_BUSINESS_CASES = (
    CASE,
    NATIVE_STOP_CASE,
    "diary_identity::remember_public_payload_covers_created_appended_and_merged",
    "diary_identity::concurrent_same_day_client_commands_preserve_each_successful_addition_once",
)
BASELINE_CASES = (
    CASE,
    "account::space_create_list_use_remove",
    "account::spaces_coordinate_with_the_owned_runtime",
    "capture_main::memory_revision_without_runtime_fails_without_local_business_fallback",
    "capture_main::residual_host_and_parser_errors_remain_user_errors",
    "diary_identity::canonical_identity_merges_legacy_tracks_without_touching_other_users_or_days",
    "diary_identity::concurrent_same_day_client_commands_preserve_each_successful_addition_once",
    "diary_identity::deleted_stable_track_uses_legacy_fallback_and_deleted_history_is_not_resurrected",
    "diary_identity::environment_key_fallback_does_not_adopt_unlocked_profile_alias",
    "diary_identity::explicit_titles_preserve_entries_and_leave_the_daily_track_unchanged",
    "diary_identity::important_remember_pending_candidates_preserve_ciphertext_and_outbox",
    "diary_identity::missing_entry_and_usage_failures_have_machine_diagnostics",
    "diary_identity::old_owner_normalized_diary_keeps_its_author_after_public_title_edit",
    "diary_identity::public_title_edit_and_blank_title_still_append_without_changing_metadata",
    "diary_identity::remember_public_payload_covers_created_appended_and_merged",
    "diary_identity::unrelated_canonical_author_fails_without_writes_or_private_output",
    "memory_revision::memory_revision_direct_is_model_free_and_follows_resolved_profile",
    "memory_revision::memory_revision_direct_respects_client_only_policy",
    "memory_revision::memory_revision_does_not_initialize_or_autostart",
    "memory_revision::memory_revision_does_not_recover_interrupted_login",
    "memory_revision::memory_revision_legacy_database_and_ambiguous_names",
    "memory_revision::memory_revision_preserves_legacy_environment_alias",
    "snapshot_lifecycle::concurrent_host_recover_cannot_interleave_with_restore_commit",
    "snapshot_lifecycle::healthy_snapshot_rescue_preserves_unparseable_current_witness",
    "snapshot_lifecycle::public_empty_recall_returns_its_query_log_identity",
    "snapshot_lifecycle::public_feedback_attribution_uses_exact_request_and_refuses_readonly_writes",
    "snapshot_lifecycle::public_snapshot_restore_reopens_runtime_and_keeps_sync_quarantined",
    "snapshot_lifecycle::public_witness_verifies_restored_chain_and_reports_tamper_without_writes",
    "snapshot_lifecycle::queued_direct_resume_rechecks_maintenance_after_library_ownership_wait",
    "snapshot_lifecycle::staged_store_open_failure_cannot_pass_boot_readiness_and_is_recoverable",
)
# This explicit remote subset excludes the diary diagnostic case containing --direct.
# The concurrent case also checks candidate HTTP idempotency directly; those checks
# are candidate Runtime evidence, not requests made by the stable CLI.
STABLE_DIARY_CASES = tuple(case for case in BASELINE_CASES
    if case.startswith("diary_identity::")
    and case != "diary_identity::missing_entry_and_usage_failures_have_machine_diagnostics")
STABLE_ASSETS = {
    "rsrs-x86_64-unknown-linux-gnu": (52491808, "c03c131c61819a5f4d132cfe328f7c1da82c2f0b21a9c87d2b8870d1541a4c3e"),
    "cli-build-x86_64-unknown-linux-gnu.json": (437, "e211d241e24408b238d994114d7c89c5e77af89fc185aaf0e4fe1c9c4ff884b2"),
    "rsrs-x86_64-unknown-linux-gnu-runtime.tar.gz": (546198, "aa4437c33089c79376410af2b83e4889a4a10b2058eed43094ddb1f40406821d"),
}
DIAGNOSTIC_CASES = (
    CASE,
    "account::spaces_coordinate_with_the_owned_runtime",
    "snapshot_lifecycle::healthy_snapshot_rescue_preserves_unparseable_current_witness",
    "snapshot_lifecycle::public_witness_verifies_restored_chain_and_reports_tamper_without_writes",
)
