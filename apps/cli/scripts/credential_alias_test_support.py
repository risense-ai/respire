"""Shared synthetic data and loaders for credential tests."""
import ast
from contextlib import closing, nullcontext
import importlib.util
import json
import sqlite3
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True
HERE = Path(__file__).parent


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


native = load('credential_test_native', 'dev-migration-keyring.py')
secret_service = load('credential_test_service', 'dev-migration-secret-service.py')
profile = load('credential_test_profile', 'dev-profile-migration-smoke.py')
legacy = profile.support
USER = 'ci-migrate-' + '1' * 16
ALIAS = 'login-' + '2' * 32
OTHER = 'login-' + '3' * 32
PASSWORD = 'synthetic-password-never-report'
SUPER = 'synthetic-super-never-report'


class MemoryKeys(native.TrackedKeys):
    credential_backend = 'native'
    def __init__(self):
        super().__init__(Path('unused'))
        self.values = {}
        self.reads = []

    def _read(self, service, slot, *, metadata_only=False):
        if metadata_only:
            return '' if (service, slot) in self.values else None
        self.reads.append((service, slot))
        return self.values.get((service, slot))

    def _put(self, service, slot, value):
        self.values[service, slot] = value

    def _remove(self, service, slot):
        self.values.pop((service, slot), None)

    def created(self, alias, super_password=SUPER):
        if super_password is not None:
            self.values['rsrs', 'super:' + alias] = super_password
        self.values['rsrs', 'pass:' + alias] = PASSWORD
