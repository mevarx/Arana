#!/usr/bin/env python3
"""Network smoke tests with isolated state; preserves successes AND failures."""
from __future__ import annotations
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT=Path(__file__).resolve().parents[1]

def main()->int:
    ap=argparse.ArgumentParser()
    ap.add_argument('--binary',type=Path,default=ROOT/'target'/'debug'/('arana.exe' if os.name=='nt' else 'arana'))
    ap.add_argument('--out',type=Path,default=ROOT/'artifacts'/'smoke')
    args=ap.parse_args()
    args.out.mkdir(parents=True,exist_ok=True)
    cases=[
        ('github-read',['github','read','rust-lang/rust','--backend','github-api']),
        ('github-search',['github','search','rust','--limit','2','--backend','github-api']),
        ('v2ex-hot',['v2ex','hot','--limit','2','--backend','v2ex-api']),
        ('bilibili-hot',['bilibili','hot','--limit','2','--backend','bilibili-api']),
        ('xueqiu-quote',['xueqiu','quote','AAPL','--backend','tencent']),
    ]
    def run(case):
        name,cli=case
        with tempfile.TemporaryDirectory(prefix='state-',dir=args.out) as state:
            start=time.monotonic()
            proc=subprocess.run([str(args.binary.resolve()),*cli,'--timeout','20'],env={**os.environ,'ARANA_HOME':state},capture_output=True,timeout=35)
            (args.out/f'{name}.stdout.json').write_bytes(proc.stdout)
            (args.out/f'{name}.stderr.log').write_bytes(proc.stderr)
            try:
                out=json.loads(proc.stdout)
                summary={'case':name,'exit_code':proc.returncode,'ok':out.get('ok'),'backend':out.get('backend'),'item_count':len(out.get('items',[])),'error':out.get('error'),'seconds':round(time.monotonic()-start,3)}
            except (ValueError,TypeError) as e:
                summary={'case':name,'exit_code':proc.returncode,'ok':False,'invalid_json':str(e),'seconds':round(time.monotonic()-start,3)}
            return summary
    with ThreadPoolExecutor(max_workers=3) as pool:
        rows=list(pool.map(run,cases))
    (args.out/'summary.json').write_text(json.dumps(rows,indent=2,ensure_ascii=False),encoding='utf-8')
    for row in rows:print(json.dumps(row,ensure_ascii=False))
    return 0 if all(r.get('ok') and r.get('item_count',0)>0 for r in rows) else 1
if __name__=='__main__':raise SystemExit(main())
