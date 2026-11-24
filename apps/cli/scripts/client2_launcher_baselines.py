"""Pinned legacy Client2 release identities and fixture digest."""
import hashlib


CLIENT2_SHA = 'be682108c555a3e5a788ad74217e088bcb25170b'
MAGPIE_SHA = 'a3ae9b3035d02b181ba7a8ddae251495d092dd63'
MAGPIE_RUNTIME_BLOB = 'ac5de49cac36a2a6a697abb9a9529b2426a9ef49'
# Official Linux x64 desktop: Client2 dev.5, produced by run 43018703263.
DESKTOP_SOURCE_SHA = CLIENT2_SHA
DESKTOP_SHA256 = 'dc74596d983ab7b99c6b6872d8438cfb2f2e7060b93f9a9ef13db629b6a5d7a2'
DESKTOP_ARCHIVE_SHA256 = '69707b8a65df1f46e73b4ea5ddc33235c861a0870f8d0260f261437228c7356a'
DESKTOP_BASELINES = {
    'host-owned-candidate': {'source': '6ffa187685734873721ab51f3a54d8b2c29cd269',
        'version': '0.2.56-dev.263',
        'binary': '4a62424ecffad8f732f5484495e1a6a5f7c6649ea7fb1809bc1b51a049f48677',
        'archive': '4a294a7de11fdaea48e534539bbfb5693c30a90e05544aea9aff344414a3c925',
        'asset_id': None, 'artifact_id': 14163499760, 'run_id': '43721147038'},
    'v2.0.0-dev.5': {'source': CLIENT2_SHA, 'binary': DESKTOP_SHA256,
        'archive': DESKTOP_ARCHIVE_SHA256, 'asset_id': 735418491, 'run_id': '43018703263'},
    'v0.2.56': {'source': '57bc070826949994c3f7ce9a64277160704bdfe5',
        'binary': '2a9b7cc40bcbf2b7b133f907e0505e2771ed12efdfa5d639a00f0e97a3bff8d7',
        'archive': '2e980f260c4593f3dbe4f7afd2cc5112c6175fa5f0f3f975901855e3e203b6b5',
        'asset_id': 707442952, 'run_id': None},
}

def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()
