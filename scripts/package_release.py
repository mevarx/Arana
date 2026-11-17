#!/usr/bin/env python3
"""Package the actually verified local binaries, documentation and safe summaries."""
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import zipfile
ROOT=Path(__file__).resolve().parents[1]

def main():
 verification=json.loads((ROOT/'artifacts/verification/verification.json').read_text())
 benchmark=json.loads((ROOT/'artifacts/benchmark.json').read_text())
 if not verification['passed']:raise RuntimeError('local verification did not pass')
 suffix='.exe' if os.name=='nt' else ''
 binaries=[ROOT/'target/release'/f'{name}{suffix}' for name in ('arana','agent-reach')]
 if hashlib.sha256(binaries[0].read_bytes()).hexdigest()!=benchmark['sha256']:raise RuntimeError('benchmark is not for the current binary')
 # Verify actual packaged executables without any developer-tool PATH entries.
 with tempfile.TemporaryDirectory(dir=ROOT/'artifacts',prefix='package-probe-') as home:
  env={**os.environ,'PATH':os.path.join(os.environ.get('SystemRoot','C:/Windows'),'System32') if os.name=='nt' else '/usr/bin:/bin','ARANA_HOME':home}
  outputs=[]
  for binary in binaries:
   result=subprocess.run([str(binary),'channels'],cwd=home,env=env,capture_output=True,timeout=10,check=True)
   catalog=json.loads(result.stdout);assert len(catalog['channels'])==14;outputs.append(result.stdout)
  assert outputs[0]==outputs[1],'alias catalog differs'
 docs=['README.md','CONTRACT_NOTES.md','LICENSE','NOTICE','schema.json','THIRD_PARTY/obscura/LICENSE','THIRD_PARTY/obscura/README.md']
 docs+=[str(p.relative_to(ROOT)) for p in sorted((ROOT/'docs').glob('*.md'))]
 summaries=['verification/verification.json','benchmark.json','smoke/summary.json','differential/summary.json','obscura-live/summary.json','obscura-live/jina-comparison.json','media-parity.json','batch-live.json','batch-review.json','tools-hardening.json','browser-acceptance.json','browser-queue.json','browser-review.json','linux-cross-check.json']
 for name in docs:assert (ROOT/name).is_file(),name
 for name in summaries:assert (ROOT/'artifacts'/name).is_file(),name
 tag='windows-x64' if os.name=='nt' else f'{sys.platform}-{platform.machine().lower()}'
 dest=ROOT/'dist'/f'arana-{tag}.zip';dest.parent.mkdir(exist_ok=True)
 with zipfile.ZipFile(dest,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=9) as archive:
  for binary in binaries:archive.write(binary,binary.name)
  for name in docs:archive.write(ROOT/name,name)
  for name in summaries:archive.write(ROOT/'artifacts'/name,'evidence/'+name)
 with zipfile.ZipFile(dest) as archive:
  assert archive.testzip() is None
  for binary in binaries:assert hashlib.sha256(archive.read(binary.name)).digest()==hashlib.sha256(binary.read_bytes()).digest()
 digest=hashlib.sha256(dest.read_bytes()).hexdigest()
 (dest.parent/'SHA256SUMS').write_text(f'{digest}  {dest.name}\n',encoding='ascii')
 print(json.dumps({'archive':str(dest),'bytes':dest.stat().st_size,'sha256':digest,'standalone_catalog_probe':True,'alias_catalog_equal':True,'zip_crc_and_binary_hashes_verified':True}))
 return 0
if __name__=='__main__':raise SystemExit(main())
