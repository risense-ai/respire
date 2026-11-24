import { positiveID, publishedAssetNames, requireValue } from "./published-artifact-values.mjs";

export function validatePublishedProcessEvidence(report, spec, target, manifestHash) {
  const expected = spec.runtime.assets.find(a => a.name === publishedAssetNames('runtime',target)[1]).sha256;
  const bundle = spec.runtime.assets.find(a => a.name === publishedAssetNames('runtime',target)[2]).sha256;
  requireValue(report.passed === true && report.runtime_mode === 'standalone_runtime' && report.standalone_runtime_evidence === true
    && report.runtime_binary_sha256 === expected && report.runtime_version === spec.runtime.version
    && Array.isArray(report.runtime_identity_receipts) && report.runtime_identity_receipts.length > 0, 'Missing actual standalone Runtime evidence: ' + target);
  for (const receipt of report.runtime_identity_receipts) {
    requireValue(receipt.manifest_sha256 === manifestHash && receipt.bundle_sha256 === bundle
      && receipt.image_sha256 === expected && typeof receipt.exe === 'string' && receipt.exe.length > 0
      && positiveID(receipt.pid) && (typeof receipt.process_start_id === 'string' && receipt.process_start_id.length > 0
        || typeof receipt.create_time === 'number' && receipt.create_time > 0)
      && typeof receipt.installation_root === 'string' && receipt.installation_root.length > 0
      && typeof receipt.data_dir === 'string' && receipt.data_dir.length > 0
      && typeof receipt.account_id === 'string' && receipt.account_id.length > 0
      && typeof receipt.instance_id === 'string' && receipt.instance_id.length > 0
      && positiveID(receipt.generation) && receipt.protocol_v === 1, 'Actual Runtime process identity differs: ' + target);
  }
  return report.runtime_identity_receipts;
}
