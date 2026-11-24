"""Load the Vault fixture and its platform credential provider."""
import importlib.util
import os
from pathlib import Path
import sys


spec = importlib.util.spec_from_file_location("vault_smoke_support", Path(__file__).with_name("dev-legacy-vault-smoke.py"))
support = importlib.util.module_from_spec(spec)
spec.loader.exec_module(support)
keyring_script = ("dev-migration-secret-service.py" if sys.platform.startswith("linux")
    and os.environ.get("RSRS_SS_FIXTURE_BUS") == "private"
    and os.environ.get("DBUS_SESSION_BUS_ADDRESS", "").startswith("unix:") else "dev-migration-keyring.py")
keyring_spec = importlib.util.spec_from_file_location("migration_native_keyring", Path(__file__).with_name(keyring_script))
keyring_backend = importlib.util.module_from_spec(keyring_spec)
keyring_spec.loader.exec_module(keyring_backend)
require, digest = support.require, support.digest
REQUIRED = ("model_cpu_real", "onememory_multiaccount_wal_migrated",
    "respire_compatibility_migrated", "legacy_db_with_explicit_current_credentials",
    "explicit_dev_endpoint_preserved", "migration_repeated_start_idempotent",
    "migration_interrupted_restart_recovered", "existing_rsrs_preserved_and_legacy_imported",
    "migrated_outbox_sync_and_independent_decrypt", "migration_wrong_key_rejected", "migration_incompatible_primary_keys_rejected",
    "migration_symlink_root_rejected", "migration_client_only_does_not_write",
    "startup_does_not_migrate", "migration_source_order_and_backup_only_idempotent",
    "restart_locked_with_native_credentials_then_host_unlock")
