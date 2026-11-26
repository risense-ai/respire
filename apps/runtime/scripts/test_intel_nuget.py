import io
import pathlib
import tempfile
import unittest
import zipfile
from unittest.mock import patch
import importlib.util
spec = importlib.util.spec_from_file_location("intel", pathlib.Path(__file__).with_name("inspect-intel-nuget.py"))
intel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(intel)

class Cases(unittest.TestCase):
    def inspect(self, change=None):
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp)
            path=root/"candidate.zip"
            rows={k:b"example" for k in intel.LIMITS}
            rows[intel.PKG+".nuspec"]=b"<package><metadata><id>Microsoft.ML.OnnxRuntime</id><version>1.20.1</version></metadata></package>"
            if change: change(rows)
            with zipfile.ZipFile(path,"w") as z:
                for k,v in rows.items(): z.writestr(k,v)
            return intel.inspect_package(path,root)
    def test_good(self): self.assertEqual(set(self.inspect()),set(intel.LIMITS))
    def test_absent(self):
        with self.assertRaises(ValueError): self.inspect(lambda rows:rows.pop(intel.LIB))
    def test_traversal(self):
        with self.assertRaises(ValueError): self.inspect(lambda rows:rows.update({"../x":b"x"}))
    def test_case_alias(self):
        with self.assertRaises(ValueError): self.inspect(lambda rows:rows.update({"license":b"x"}))
    def test_wrong_version(self):
        def change(rows):
            k=intel.PKG+".nuspec"
            rows[k]=rows[k].replace(b"1.20.1",b"1.28.0")
        with self.assertRaises(ValueError): self.inspect(change)
    def test_limit(self):
        with patch.dict(intel.LIMITS,LICENSE=1),self.assertRaises(ValueError): self.inspect()
    def test_signature_gate(self):
        for s in ["","Signature verification unavailable","Successfully verified package"]:
            with self.assertRaises(ValueError): intel.signature_evidence(s)
    def test_redirect(self):
        with self.assertRaises(ValueError): intel.NoRedirect().redirect_request()
