"""Trusted base-side commit check and publication operations. Treat media as byte strings."""
import base64
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import zipfile

REPO = 'risense-ai/respire'
LIMIT = 12 * 1024 * 1024
FILES = {'en.png','en.mp4','zh.png','zh.mp4'}
SECRET = re.compile(rb'-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----|github_pat_[A-Za-z0-9_]{25,}|gh[pousr]_[A-Za-z0-9]{30,}|AKIA[A-Z0-9]{16}')


def api(method, path, payload=None, raw=False):
    args = ['gh','api','--method',method,'repos/'+REPO+'/'+path]
    if payload is not None:
        args.extend(['--input','-'])
    if raw:
        # The job timeout also bounds a stalled transport.
        with subprocess.Popen(args, stdout=subprocess.PIPE) as child:
            try:
                data = child.stdout.read(LIMIT + 1)
                if len(data) > LIMIT or child.wait(timeout=60):
                    raise ValueError('invalid artifact download')
                return data
            finally:
                child.kill()
    data = subprocess.check_output(args,input=None if payload is None else json.dumps(payload).encode(),timeout=60)
    return data if raw else json.loads(data)


def selected(pr, head):
    return (pr['state']=='open' and pr['head']['repo']['full_name']==REPO and
            pr['head']['sha']==head and pr['base']['ref']=='main')



FIXTURE_BODY = '<!-- respire-ui-preview:fixture -->'


def fixture_body(pr, head, url):
    if not (
        selected(pr, head)
        and pr['draft']
        and pr['head']['ref'].startswith('test/client2-preview-')
        and any(label['name'] == 'ui-preview' for label in pr['labels'])
        and pr['body'] == FIXTURE_BODY
    ):
        return None
    media = '\n\n'.join(
        f'![{locale} screenshot]({url}{locale}.png?raw=true) · [{locale} MP4]({url}{locale}.mp4)'
        for locale in ('en', 'zh')
    )
    return f'{FIXTURE_BODY}\n\nPreview of `{head}`\n\n{media}'


def publish_fixture_body(number, head, url):
    path = f'pulls/{number}'
    body = fixture_body(api('GET', path), head, url)
    if body is None:
        return
    api('PATCH', path, {'body': body})
    result = api('GET', path)
    if not selected(result, head) or result['body'] != body:
        raise ValueError('fixture body publication not verified')
    print(f'Published both locales to the fixture PR body at {head}')

def inspect(raw):
    if len(raw)>LIMIT:
        raise ValueError('oversized archive')
    with zipfile.ZipFile(io.BytesIO(raw)) as z:
        items=z.infolist()
        if len(items)!=4 or {i.filename for i in items}!=FILES or sum(i.file_size for i in items)>LIMIT:
            raise ValueError('invalid media inventory')
        result={}
        for i in items:
            if not 32<=i.file_size<=5*1024*1024 or i.compress_type not in (0,8):
                raise ValueError('invalid media size or compression')
            data=z.read(i)
            png=i.filename.endswith('.png')
            valid=(data.startswith(b'\x89PNG\r\n\x1a\n') and data[12:16]==b'IHDR' and data[-8:-4]==b'IEND') if png else data[4:8]==b'ftyp'
            if not valid or SECRET.search(data):
                raise ValueError('invalid media or secret pattern')
            result[i.filename]=data
    return result


def main():
    number=int(os.environ['PR_NUMBER'])
    head=os.environ['PR_HEAD']
    if os.environ['GITHUB_REPOSITORY']!=REPO or not re.fullmatch('[0-9a-f]{40}',head):
        raise ValueError('invalid source identity')
    run=int(os.environ['GITHUB_RUN_ID'])
    attempt=int(os.environ['GITHUB_RUN_ATTEMPT'])
    aid=int(os.environ['MEDIA_ID'])
    artifact=api('GET',f'actions/artifacts/{aid}')
    if artifact['workflow_run']['id']!=run or artifact['expired'] or artifact['name']!=f'ui-{run}-{attempt}-{number}-{head}' or artifact['size_in_bytes']>LIMIT:
        raise ValueError('artifact binding mismatch')
    raw=api('GET',f'actions/artifacts/{aid}/zip',raw=True)
    if 'sha256:'+hashlib.sha256(raw).hexdigest()!=artifact['digest']:
        raise ValueError('archive digest mismatch')
    media=inspect(raw)
    if not selected(api('GET',f'pulls/{number}'),head):
        raise ValueError('stale preview')
    refs=api('GET','git/matching-refs/heads/ui-previews')
    previous=next((r['object']['sha'] for r in refs if r['ref']=='refs/heads/ui-previews'),None)
    tree={}
    if previous:
        tree['base_tree']=api('GET','git/commits/'+previous)['tree']['sha']
    folder=f'pr-{number}/{head}'
    entries=[]
    for name,data in sorted(media.items()):
        blob=api('POST','git/blobs',{'encoding':'base64','content':base64.b64encode(data).decode()})
        entries.append({'path':folder+'/'+name,'mode':'100644','type':'blob','sha':blob['sha']})
    tree['tree']=entries
    tree_id=api('POST','git/trees',tree)['sha']
    commit=api('POST','git/commits',{'message':f'UI preview for #{number} at {head}','tree':tree_id,'parents':[previous] if previous else []})['sha']
    if previous:
        api('PATCH','git/refs/heads/ui-previews',{'sha':commit,'force':False})
    else:
        api('POST','git/refs',{'ref':'refs/heads/ui-previews','sha':commit})
    url=f'https://github.com/{REPO}/blob/{commit}/{folder}/'
    links = '\n\n'.join(f'[{locale} PNG]({url}{locale}.png?raw=true) · [{locale} MP4]({url}{locale}.mp4)' for locale in ('en','zh'))
    publish_fixture_body(number, head, url)
    api('POST','check-runs',{'name':'Client2 preview','head_sha':head,
        'external_id':f'{run}/{attempt}/{aid}', 'status':'completed','conclusion':'success',
        'output':{'title':f'Preview of {head}', 'summary':links}})


if __name__=='__main__':
    main()
