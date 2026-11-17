#!/usr/bin/env python3
"""Run the pinned Python and Rust CLIs side by side; report exact non-timing diffs."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import tempfile
ROOT=Path(__file__).resolve().parents[1]

def without_timing(out):
 out=json.loads(json.dumps(out))
 if isinstance(out.get('meta'),dict):out['meta'].pop('elapsed_ms',None)
 return out

def differences(a,b,path='$'):
 if type(a) is not type(b):return [path+': type differs']
 if isinstance(a,dict):
  result=[]
  for k in sorted(a.keys()|b.keys()):
   if k not in a or k not in b:result.append(path+'.'+k+': missing')
   else:result.extend(differences(a[k],b[k],path+'.'+k))
  return result
 if isinstance(a,list):
  result=[] if len(a)==len(b) else [path+': length differs']
  for i,(x,y) in enumerate(zip(a,b)):result.extend(differences(x,y,f'{path}[{i}]'))
  return result
 return [] if a==b else [path+': value differs']

def main():
 p=argparse.ArgumentParser()
 p.add_argument('--binary',type=Path,default=ROOT/'target/debug'/('arana.exe' if os.name=='nt' else 'arana'))
 p.add_argument('--python',type=Path,default=ROOT/'.upstream-venv'/('Scripts/python.exe' if os.name=='nt' else 'bin/python'))
 p.add_argument('--upstream',type=Path,default=ROOT.parent/'arana-upstream-reference')
 p.add_argument('--out',type=Path,default=ROOT/'artifacts/differential')
 p.add_argument('--only',default='')
 args=p.parse_args();args.out.mkdir(parents=True,exist_ok=True)
 cases=[('v2ex-read',['v2ex','read','https://www.v2ex.com/t/1000','--backend','v2ex-api']),('v2ex-user',['v2ex','user','Livid','--backend','v2ex-api']),('github-read',['github','read','rust-lang/rust','--backend','github-api']),('xueqiu-quote',['xueqiu','quote','AAPL','--backend','tencent']),
 ('v2ex-hot',['v2ex','hot','--limit','2','--backend','v2ex-api']),('v2ex-node',['v2ex','node','python','--limit','2','--backend','v2ex-api']),
 ('github-search',['github','search','rust','--limit','2','--backend','github-api']),('xueqiu-search',['xueqiu','search','AAPL','--limit','2','--backend','tencent']),
 ('bilibili-read',['bilibili','read','BV1GJ411x7h7','--backend','bilibili-api']),('bilibili-hot',['bilibili','hot','--limit','2','--backend','bilibili-api']),
 ('web-read-jina',['web','read','https://example.com','--backend','jina']),('web-search-exa',['web','search','Rust programming language','--limit','2','--backend','exa'])]
 if args.only:cases=[c for c in cases if c[0] in args.only.split(',')]
 rows=[]
 for name,cli in cases:
  with tempfile.TemporaryDirectory(dir=args.out,prefix='state-') as home:
   env={**os.environ,'HOME':home,'USERPROFILE':home,'ARANA_HOME':str(Path(home)/'.arana'),'XDG_CONFIG_HOME':str(Path(home)/'.config'),'PYTHONPATH':str(args.upstream),'PYTHONIOENCODING':'utf-8'}
   for key in ('GITHUB_TOKEN','GH_TOKEN','GROQ_API_KEY','OPENAI_API_KEY'):env.pop(key,None)
   commands={'python':[str(args.python),'-m','agent_reach.cli',*cli,'--timeout','30'],'rust':[str(args.binary),*cli,'--timeout','30']}
   def run(pair):
    label,command=pair
    result=subprocess.run(command,env=env,cwd=home,capture_output=True,timeout=50)
    (args.out/f'{name}.{label}.stdout.json').write_bytes(result.stdout);(args.out/f'{name}.{label}.stderr.log').write_bytes(result.stderr)
    return label,(result.returncode,json.loads(result.stdout))
   with ThreadPoolExecutor(max_workers=2) as pool:results=dict(pool.map(run,commands.items()))
   pycode,py=results['python'];rscode,rs=results['rust'];diff=differences(without_timing(py),without_timing(rs))
   row={'case':name,'python_exit':pycode,'rust_exit':rscode,'python_ok':py.get('ok'),'rust_ok':rs.get('ok'),'exact_non_timing_match':not diff,'diff_paths':diff,'passed':py.get('ok') is True and rs.get('ok') is True and not diff}
   rows.append(row);print(json.dumps(row),flush=True)
 (args.out/'summary.json').write_text(json.dumps(rows,indent=2),encoding='utf-8')
 return 0 if rows and all(r['passed'] for r in rows) else 1
if __name__=='__main__':raise SystemExit(main())
