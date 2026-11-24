import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { REPO, hash, positive } from "./release-recovery-identity.mjs";

export function recoveryArtifact(artifacts,name,job,owner,{failed=false}={}){
  assert.equal(job.status,'completed');assert.equal(job.conclusion,failed?'failure':'success');
  assert.equal(job.run_id,Number(owner.run_id));assert.equal(job.run_attempt,Number(owner.run_attempt));assert.equal(job.head_sha,owner.sha);
  const found=artifacts.filter(a=>a.name===name);assert.equal(found.length,1,'Missing or duplicate artifact: '+name);
  const a=found[0];
  assert.ok(positive(a.id)&&positive(a.size_in_bytes));assert.equal(a.expired,false);assert.match(a.digest,/^sha256:[a-f0-9]{64}$/);
  assert.equal(a.workflow_run?.id,Number(owner.run_id));assert.equal(a.workflow_run.head_sha,owner.sha);
  const times=[job.started_at,a.created_at,a.updated_at,job.completed_at].map(Date.parse);
  assert.ok(times.every(Number.isFinite)&&times.every((n,i)=>i===0||n>=times[i-1]),'Artifact does not belong to the selected producer job/attempt');
  return {id:String(a.id),name:a.name,size:a.size_in_bytes,digest:a.digest,job_id:job.id,owner,conclusion:job.conclusion};
}

// Download through gh, check the original ZIP digest, then extract regular files only.
export function downloadRecoveryArtifact(a,destination,{transport=execFileSync}={}){
  if(transport===execFileSync)assert.ok(process.env.GH_TOKEN,'Explicit read token required');
  const bytes=transport('gh',['api','--hostname','github.com',`repos/${REPO}/actions/artifacts/${a.id}/zip`],{maxBuffer:1024*1024*1024});
  assert.equal(bytes.length,a.size);assert.equal('sha256:'+hash(bytes),a.digest,'Original artifact ZIP digest differs');
  mkdirSync(destination,{recursive:true});const zip=join(destination,'.download.zip');writeFileSync(zip,bytes);
  try{execFileSync(process.platform==='win32'?'py':'python3',[...(process.platform==='win32'?['-3']:[]),'-I','-c',`
import pathlib,stat,sys,zipfile
root=pathlib.Path(sys.argv[2]).resolve()
with zipfile.ZipFile(sys.argv[1]) as z:
 names=z.namelist(); assert len(names)==len(set(names)), 'Duplicate ZIP members'
 assert sum(e.file_size for e in z.infolist()) <= 1024**3, 'Oversized ZIP payload'
 for e in z.infolist():
  p=pathlib.PurePosixPath(e.filename)
  assert not p.is_absolute() and '..' not in p.parts and '\\\\' not in e.filename and ':' not in e.filename, 'Unsafe ZIP path'
  assert stat.S_IFMT(e.external_attr>>16) in (0,stat.S_IFREG,stat.S_IFDIR), 'Non-regular ZIP entry'
  target=root.joinpath(*p.parts).resolve(); assert target.is_relative_to(root) and target!=root, 'Escaped ZIP member'
  if e.is_dir(): target.mkdir(parents=True,exist_ok=True); continue
  target.parent.mkdir(parents=True,exist_ok=True)
  with target.open('xb') as out: out.write(z.read(e))
`,zip,destination],{stdio:'inherit'});}finally{rmSync(zip);}
}
